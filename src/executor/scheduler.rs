use std::sync::Arc;
use std::any::Any;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crossbeam::deque::{Injector, Stealer};
use crossbeam::queue::SegQueue;
use crossbeam::sync::Unparker;
use crossbeam_deque::Steal;
use futures::FutureExt;

use crate::executor::Task;
use crate::executor::join_handle::JoinHandle;

/// Coordinates and distributes tasks accross all worker threads
/// uses `crossbeam::deque::Injector`
pub struct Scheduler {
    /// The global injector queue for tasks that don't have a specific worker affinity.
    pub(crate) queue: Arc<Injector<Arc<Task>>>,
    /// Thread-local worker stealers for work-stealing from other threads.
    pub stealers: Vec<Stealer<Arc<Task>>>,
    /// Unparkers for waking up sleeping workers.
    pub unparkers: Vec<Unparker>,
    /// Tracks the number of workers currently parked.
    pub sleeping_workers: AtomicUsize,
    /// Tracks the number of workers currently searching for work (safely throttles notifications).
    pub searching_workers: AtomicUsize,
    /// Round-robin cursor for notifying workers.
    pub notify_cursor: AtomicUsize,
    /// Shutdown signal.
    pub(crate) shutdown: AtomicBool,
    /// Bucketized task pools for zero-allocation recycling (indexed by power-of-two size).
    pub task_pools: [Arc<SegQueue<Arc<Task>>>; 11],
}

impl Scheduler {
    pub(crate) fn new(stealers: Vec<Stealer<Arc<Task>>>, unparkers: Vec<Unparker>) -> Self {
        Self {
            queue: Arc::new(Injector::new()),
            stealers,
            unparkers,
            sleeping_workers: AtomicUsize::new(0),
            searching_workers: AtomicUsize::new(0),
            notify_cursor: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
            task_pools: std::array::from_fn(|_| Arc::new(SegQueue::new())),
        }
    }

    /// Spawns a future onto the runtime and returns a JoinHandle to its result.
    pub fn spawn<F, T>(self: &Arc<Self>, future: F) -> JoinHandle<T>
    where 
        F: Future<Output = T> + Send + 'static,
        T: Any + Send + 'static,
    {
        let wrapped_future = async move {
            let res = std::panic::AssertUnwindSafe(future).catch_unwind().await;
            
            // Get our own task header to write the result
            let task = crate::executor::context::CURRENT_TASK.with(|c| c.borrow().clone().expect("Task executed outside of context"));

            // Type-erased result storage
            let boxed_res = res.map(|val| Box::new(val) as Box<dyn Any + Send>);
            
            unsafe {
                *task.result.get() = Some(boxed_res);
            }

            // Mark a READY and wake joiner
            task.join_state.store(crate::executor::task::JOIN_STATE_READY, Ordering::Release);
            if let Some(waker) = unsafe { &mut *task.join_waker.get() }.take() {
                waker.wake();
            }
        };

        let task = self.spawn_internal_ref(wrapped_future);
        JoinHandle::new(task)
    }

    fn spawn_internal_ref<F>(self: &Arc<Self>, future: F) -> Arc<Task>
    where F: Future<Output = ()> + Send + 'static
    {
        let layout = std::alloc::Layout::new::<F>();
        if let Some(idx) = Self::pool_index(layout) {
            // 1. Try Thread-Local pool
            let local_task = crate::executor::context::LOCAL_TASK_POOL.with(|p| {
                p[idx].borrow_mut().pop()
            });
            if let Some(task) = local_task {
                crate::executor::task::Task::reuse(&task, future);
                self.inject(task.clone());
                return task;
            }

            // 2. Fallback to Global pool
            if let Some(task) = self.task_pools[idx].pop() {
                crate::executor::task::Task::reuse(&task, future);
                self.inject(task.clone());
                return task;
            }
        }
        
        let pooled_layout = if let Some(idx) = Self::pool_index(layout) {
             Some(std::alloc::Layout::from_size_align(32 << idx, 16).unwrap())
        } else {
             None
        };

        let task = Task::new(future, self.clone(), pooled_layout);
        self.inject(task.clone());
        task
    }


    pub(crate) fn inject(&self, task: Arc<Task>) {
        self.queue.push(task);
        self.notify();
    }

    pub(crate) fn notify(&self) {
        // Only unpark if no one is searching. This significantly reduces kernel transitions.
        if self.searching_workers.load(Ordering::Acquire) == 0 {
            let idx = self.notify_cursor.fetch_add(1, Ordering::Relaxed) % self.unparkers.len();
            self.unparkers[idx].unpark();
        }
    }


    pub(crate) fn steal(&self) -> Steal<Arc<Task>> {
        self.queue.steal()
    }

    /// Maps a memory layout to a specific size-bucket in the task pool.
    /// Buckets cover powers of two from 32B (Index 0) to 32KB (Index 10).
    pub(crate) fn pool_index(layout: std::alloc::Layout) -> Option<usize> {
        if layout.align() > 16 || layout.size() > 32768 {
            return None;
        }
        if layout.size() == 0 {
            return None;
        }
        let size = std::cmp::max(32, layout.size().next_power_of_two());
        Some((size.trailing_zeros() as usize) - 5)
    }
}