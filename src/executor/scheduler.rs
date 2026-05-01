use std::sync::Arc;
use std::any::Any;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::alloc::Layout;

use crossbeam::deque::{Injector, Stealer};
use crossbeam::queue::ArrayQueue;
use crossbeam::sync::Unparker;
use crossbeam::utils::CachePadded;
use crossbeam_deque::Steal;
use futures::FutureExt;

use crate::executor::Task;
use crate::executor::join_handle::JoinHandle;
use crate::executor::context::{CURRENT_TASK, LOCAL_TASK_POOL};

/// The global coordinator for task distribution and resource recycling.
/// 
/// The `Scheduler` coordinates the lifecycle of all tasks in the runtime, 
/// providing load balancing and memory-efficient resource management.
/// 
/// ## Task Distribution Strategy
/// 
/// Taiga implements a multi-level task discovery hierarchy:
/// 
/// 1.  **LIFO Slot**: A single-task priority slot for tasks woken by the 
///     current thread. This maximizes CPU cache locality.
/// 2.  **Local Deque**: A per-worker lock-free deque for low-latency local 
///     task management.
/// 3.  **Work-Stealing**: A mechanism to migrate tasks from over-subscribed 
///     local deques or the global injector queue to idle workers.
/// 
/// ## Resource Recycling
/// 
/// To minimize heap allocation overhead, Taiga utilizes **Object Pools** 
/// (bucketized by memory layout). Upon task completion, `Arc<Task>` objects 
/// are returned to these pools. Subsequent `spawn` operations prioritize 
/// reusing these allocations, reducing allocator contention and pressure 
/// on the memory subsystem.
pub struct Scheduler {
    /// The global injector queue for tasks that don't have a specific worker affinity.
    pub(crate) queue: Arc<Injector<Arc<Task>>>,
    /// Thread-local worker stealers for work-stealing from other threads.
    pub stealers: Vec<Stealer<Arc<Task>>>,
    /// Unparkers for waking up sleeping workers.
    pub unparkers: Vec<Unparker>,
    /// Tracks the number of workers currently parked.
    pub sleeping_workers: CachePadded<AtomicUsize>,
    /// Tracks the number of workers currently searching for work.
    /// This is used to throttle notifications and prevent "thundering herd" issues.
    pub searching_workers: CachePadded<AtomicUsize>,
    /// Round-robin cursor for notifying workers.
    pub notify_cursor: CachePadded<AtomicUsize>,
    /// Shutdown signal.
    pub(crate) shutdown: CachePadded<AtomicBool>,
    /// Bucketized task pools for zero-allocation recycling.
    /// 
    /// Pools are indexed by the size of the task. We have 11 buckets ranging 
    /// from 32 bytes to 32 kilobytes.
    pub task_pools: [Arc<ArrayQueue<Arc<Task>>>; 11],
    /// Callback to wake the reactor if a worker is blocked polling I/O.
    pub(crate) reactor_notifier: Box<dyn Fn() + Send + Sync>,
}

impl Scheduler {
    /// Creates a new scheduler.
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

    /// Spawns a future onto the runtime and returns a [`JoinHandle`] to its result.
    /// 
    /// This method is responsible for taking your `async` block and turning it 
    /// into a [`Task`] that the runtime can manage. It also handles panics 
    /// gracefully so that one failing task doesn't crash the whole runtime.
    pub fn spawn<F, T>(self: &Arc<Self>, future: F) -> JoinHandle<T>
    where 
        F: Future<Output = T> + Send + 'static,
        T: Any + Send + 'static,
    {
        let wrapped_future = async move {
            // We use AssertUnwindSafe to catch panics inside the task
            let res = std::panic::AssertUnwindSafe(future).catch_unwind().await;
            
            // Get our own task header to write the result
            let task = CURRENT_TASK.with(|c| 
                c.borrow().clone().expect("Task executed outside of context")
            );

            // Store the result (or the panic error) in the task's result slot
            let boxed_res = res.map(|val| Box::new(val) as Box<dyn Any + Send>);
            
            // SAFETY: We are currently executing on the task's thread and have 
            // exclusive access to the task's internal state during the final 
            // stage of its execution.
            unsafe {
                *task.result.get() = Some(boxed_res);
            }

            // Signal that we are done and wake up anyone waiting on the JoinHandle.
            // Release ordering ensures that the results written to `task.result` 
            // above are visible to any thread that performs an Acquire load 
            // of the join_state.
            task.join_state.store(crate::executor::task::JOIN_STATE_READY, Ordering::Release);
            task.join_waker.wake();
        };

        let task = self.spawn_internal_ref(wrapped_future);
        JoinHandle::new(task)
    }

    /// The internal heart of task creation.
    /// 
    /// This method first checks if there's a compatible task in the pools 
    /// (recycling) before allocating a new one.
    fn spawn_internal_ref<F>(self: &Arc<Self>, future: F) -> Arc<Task>
    where F: Future<Output = ()> + Send + 'static
    {
        let layout = Layout::new::<F>();
        if let Some(idx) = Self::pool_index(layout) {
            // 1. Try Thread-Local pool (FASTEST, no sharing with other threads)
            let local_task = LOCAL_TASK_POOL.with(|p| {
                p[idx].borrow_mut().pop()
            });
            if let Some(task) = local_task {
                Task::reuse(&task, future);
                self.inject(task.clone());
                return task;
            }

            // 2. Fallback to Global pool (Shared with other threads)
            if let Some(task) = self.task_pools[idx].pop() {
                Task::reuse(&task, future);
                self.inject(task.clone());
                return task;
            }
        }
        
        // 3. If no poolable task found, create a new one from scratch
        let pooled_layout = if let Some(idx) = Self::pool_index(layout) {
             Some(Layout::from_size_align(32 << idx, 16).unwrap())
        } else {
             None
        };

        let task = Task::new(future, self.clone(), pooled_layout);
        self.inject(task.clone());
        task
    }


    /// Injects a task into the global queue and notifies available workers.
    pub(crate) fn inject(&self, task: Arc<Task>) {
        self.queue.push(task);
        self.notify();
    }

    /// Signals that new work is available.
    /// 
    /// To avoid wasting energy, we only wake up workers if they are actually 
    /// needed. We try to keep a small number of workers "searching" for work.
    pub(crate) fn notify(&self) {
        // Use Acquire to check the number of searching workers to ensure we 
        // see the most up-to-date count before deciding to wake another worker.
        if self.searching_workers.load(Ordering::Acquire) < 2 {
            let num_unparkers = self.unparkers.len();
            if num_unparkers > 0 {
                // Use Relaxed for the notify cursor as it's just a round-robin 
                // counter for load balancing and doesn't affect memory safety.
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

    /// Maps a memory layout (size/alignment) to a specific bucket in our pool.
    /// 
    /// We use power-of-two buckets (32B, 64B, 128B... up to 32KB).
    pub(crate) fn pool_index(layout: Layout) -> Option<usize> {
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
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 0]>()), None);
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 32]>()), Some(0));
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 64]>()), Some(1));
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 32768]>()), Some(10));
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 32769]>()), None);
        assert_eq!(Scheduler::pool_index(Layout::from_size_align(32, 32).unwrap()), None);
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
            Steal::Success(t) => assert_eq!(Arc::as_ptr(&t), Arc::as_ptr(&task)),
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
        let layout = Layout::new::<[u8; 32]>();
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
        assert_eq!(Scheduler::pool_index(Layout::from_size_align(32, 32).unwrap()), None);
        // Size > 32768 should return None
        assert_eq!(Scheduler::pool_index(Layout::from_size_align(32769, 16).unwrap()), None);
        
        // Powers of two mapping
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 32]>()), Some(0)); // 32
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 33]>()), Some(1)); // 64
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 64]>()), Some(1));
        assert_eq!(Scheduler::pool_index(Layout::new::<[u8; 65]>()), Some(2)); // 128
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