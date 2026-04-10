use std::sync::Arc;
use std::any::Any;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crossbeam::deque::{Injector, Stealer};
use crossbeam::queue::ArrayQueue;
use crossbeam::sync::Unparker;
use crossbeam::utils::CachePadded;
use crossbeam_deque::Steal;
use futures::FutureExt;

use crate::executor::Task;
use crate::executor::join_handle::JoinHandle;
use crate::executor::context::CURRENT_TASK;

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
    pub sleeping_workers: CachePadded<AtomicUsize>,
    /// Tracks the number of workers currently searching for work (safely throttles notifications).
    pub searching_workers: CachePadded<AtomicUsize>,
    /// Round-robin cursor for notifying workers.
    pub notify_cursor: CachePadded<AtomicUsize>,
    /// Shutdown signal.
    pub(crate) shutdown: CachePadded<AtomicBool>,
    /// Bucketized task pools for zero-allocation recycling (indexed by power-of-two size).
    pub task_pools: [Arc<ArrayQueue<Arc<Task>>>; 11],
    /// Callback to wake the reactor if a worker is blocked polling I/O.
    pub(crate) reactor_notifier: Box<dyn Fn() + Send + Sync>,
}

impl Scheduler {
    pub(crate) fn new(
        stealers: Vec<Stealer<Arc<Task>>>, 
        unparkers: Vec<Unparker>, 
        reactor_notifier: Box<dyn Fn() + Send + Sync>
    ) -> Self {
        Self {
            queue: Arc::new(Injector::new()),
            stealers,
            unparkers,
            sleeping_workers: CachePadded::new(AtomicUsize::new(0)),
            searching_workers: CachePadded::new(AtomicUsize::new(0)),
            notify_cursor: CachePadded::new(AtomicUsize::new(0)),
            shutdown: CachePadded::new(AtomicBool::new(false)),
            task_pools: std::array::from_fn(|_| Arc::new(ArrayQueue::new(1024))),
            reactor_notifier,
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
            let task = CURRENT_TASK.with(|c| 
                c.borrow().clone().expect("Task executed outside of context")
            );

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
                Task::reuse(&task, future);
                self.inject(task.clone());
                return task;
            }

            // 2. Fallback to Global pool
            if let Some(task) = self.task_pools[idx].pop() {
                Task::reuse(&task, future);
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
        // Relaxed threshold: Allow unparking if searching workers < 2.
        // This prevents the "single searcher" bottleneck during high-load bursts.
        if self.searching_workers.load(Ordering::Acquire) < 2 {
            let num_unparkers = self.unparkers.len();
            if num_unparkers > 0 {
                let idx = self.notify_cursor.fetch_add(1, Ordering::Relaxed) % num_unparkers;
                self.unparkers[idx].unpark();
            }
            // Wake the reactor in case the only available worker is parked inside epoll_wait
            (self.reactor_notifier)();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::ready;

    #[test]
    fn test_pool_index() {
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 0]>()), None);
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 32]>()), Some(0));
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 64]>()), Some(1));
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 32768]>()), Some(10));
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 32769]>()), None);
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::from_size_align(32, 32).unwrap()), None);
    }

    #[test]
    fn test_scheduler_new() {
        let reactor_notifier = Box::new(|| {});
        let scheduler = Scheduler::new(vec![], vec![], reactor_notifier);
        assert_eq!(scheduler.unparkers.len(), 0);
        assert!(!scheduler.shutdown.load(Ordering::SeqCst));
    }

    #[test]
    fn test_inject_and_steal() {
        let reactor_notifier = Box::new(|| {});
        let scheduler = Arc::new(Scheduler::new(vec![], vec![], reactor_notifier));
        let task = Task::new(ready(()), scheduler.clone(), None);
        
        scheduler.inject(task.clone());
        let stolen = scheduler.steal();
        match stolen {
            crossbeam_deque::Steal::Success(t) => assert_eq!(Arc::as_ptr(&t), Arc::as_ptr(&task)),
            _ => panic!("Failed to steal task"),
        }
    }

    #[test]
    fn test_notify_logic() {
        let notified = Arc::new(AtomicBool::new(false));
        let notified_clone = notified.clone();
        let reactor_notifier = Box::new(move || {
            notified_clone.store(true, Ordering::SeqCst);
        });
        
        let parker1 = crossbeam::sync::Parker::new();
        let unparker1 = parker1.unparker().clone();
        let parker2 = crossbeam::sync::Parker::new();
        let unparker2 = parker2.unparker().clone();
        let scheduler = Scheduler::new(vec![], vec![unparker1, unparker2], reactor_notifier);
        
        // Initial cursor is 0
        scheduler.searching_workers.store(0, Ordering::SeqCst);
        scheduler.notify();
        assert!(notified.load(Ordering::SeqCst));
        assert_eq!(scheduler.notify_cursor.load(Ordering::SeqCst), 1);
        
        notified.store(false, Ordering::SeqCst);
        scheduler.notify();
        assert!(notified.load(Ordering::SeqCst));
        assert_eq!(scheduler.notify_cursor.load(Ordering::SeqCst), 2);
        
        notified.store(false, Ordering::SeqCst);
        scheduler.searching_workers.store(2, Ordering::SeqCst);
        scheduler.notify();
        assert!(!notified.load(Ordering::SeqCst));
    }

    #[test]
    fn test_spawn_and_reuse() {
        let reactor_notifier = Box::new(|| {});
        let scheduler = Arc::new(Scheduler::new(vec![], vec![], reactor_notifier));
        
        // 1. Basic spawn
        let _handle = scheduler.spawn(async { 42 });
        assert!(matches!(scheduler.steal(), crossbeam_deque::Steal::Success(_)));

        // 2. Reuse via LOCAL_TASK_POOL
        let layout = std::alloc::Layout::new::<[u8; 32]>();
        let idx = Scheduler::pool_index(layout).unwrap();
        
        let dummy_task = Task::new(async {}, scheduler.clone(), Some(layout));
        crate::executor::context::LOCAL_TASK_POOL.with(|p| {
            p[idx].borrow_mut().push(dummy_task.clone());
        });

        let reused_task = scheduler.spawn_internal_ref(async {});
        assert!(Arc::ptr_eq(&reused_task, &dummy_task));
        assert!(matches!(scheduler.steal(), crossbeam_deque::Steal::Success(_)));

        // 3. Fallback to global task_pools
        let dummy_task_global = Task::new(async {}, scheduler.clone(), Some(layout));
        let _ = scheduler.task_pools[idx].push(dummy_task_global.clone());
        
        let reused_task_global = scheduler.spawn_internal_ref(async {});
        assert!(Arc::ptr_eq(&reused_task_global, &dummy_task_global));
        assert!(matches!(scheduler.steal(), crossbeam_deque::Steal::Success(_)));
    }

    #[test]
    fn test_pool_index_boundaries() {
        // Alignment > 16 should return None
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::from_size_align(32, 32).unwrap()), None);
        // Size > 32768 should return None
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::from_size_align(32769, 16).unwrap()), None);
        
        // Powers of two mapping
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 32]>()), Some(0)); // 32
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 33]>()), Some(1)); // 64
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 64]>()), Some(1));
        assert_eq!(Scheduler::pool_index(std::alloc::Layout::new::<[u8; 65]>()), Some(2)); // 128
    }

    #[test]
    fn test_inject_notifications() {
        let notified = Arc::new(AtomicBool::new(false));
        let notified_clone = notified.clone();
        let reactor_notifier = Box::new(move || {
            notified_clone.store(true, Ordering::SeqCst);
        });
        
        let scheduler = Arc::new(Scheduler::new(vec![], vec![], reactor_notifier));
        let task = Task::new(async {}, scheduler.clone(), None);
        
        scheduler.inject(task);
        assert!(notified.load(Ordering::SeqCst));
    }
}