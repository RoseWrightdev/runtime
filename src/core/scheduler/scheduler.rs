use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crossbeam::utils::CachePadded;

use crate::core::executor::context::Context as ExecutorContext;

use crate::core::reactor::Reactor;
use crate::core::scheduler::global_queue::GlobalQueue;
use crate::core::scheduler::join::JoinHandle;
use crate::core::scheduler::task::{Task, TaskRef};
use crate::core::scheduler::worker_pool::Pool;

#[repr(align(64))]
pub(crate) struct Scheduler {
    pub(crate) global_queue: CachePadded<GlobalQueue>,
    worker_pool: CachePadded<Pool>,
    shutdown: CachePadded<AtomicBool>,
    pub(crate) searching_workers: AtomicUsize,
    parked_workers: AtomicUsize,
    pub(crate) reactor: Arc<Reactor>,
    pub(crate) notifying: AtomicBool,
}

impl Scheduler {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::new_with_workers(num_cpus::get())
    }

    pub fn new_with_workers(num_workers: usize) -> Self {
        Scheduler {
            global_queue: CachePadded::new(GlobalQueue::new()),
            worker_pool: CachePadded::new(Pool::new(num_workers)),
            shutdown: CachePadded::new(AtomicBool::new(false)),
            searching_workers: AtomicUsize::new(0),
            parked_workers: AtomicUsize::new(0),
            reactor: Arc::new(Reactor::new().expect("Failed to create Reactor")),
            notifying: AtomicBool::new(false),
        }
    }

    pub fn spawn_internal<F, T>(self: &Arc<Self>, future: F) -> JoinHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        if self.shutdown.load(Ordering::Acquire) {
            panic!("Spawned on shutdown scheduler");
        }

        let scheduler = self.clone();
        let task = Task::spawn(future, scheduler);
        let join_handle = JoinHandle::new(task.clone());

        // Try to push to the local worker LIFO slot first
        if !ExecutorContext::try_push_local(task.clone(), self) {
            self.global_queue.push(task);
            self.notify_adaptive();
        }

        join_handle
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        // Wake up all workers to see the shutdown signal.
        // unpark_all() wakes workers parked in std::thread::park().
        self.worker_pool.unpark_all();
        // wakeup() pulls the "Guardian" worker out of mio::poll().
        self.reactor.wakeup();
    }

    pub fn join(&self) {
        self.worker_pool.join();
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    pub fn steal_global(&self) -> Option<TaskRef> {
        self.global_queue.steal()
    }

    pub fn start(self: &Arc<Self>) {
        self.worker_pool.start(self.clone());
    }


    pub fn notify_adaptive(&self) {
        let searching = self.searching_workers.load(Ordering::Acquire);

        // ALWAYS unpark at least one worker when a task is pushed to the global queue.
        // This ensures that we never miss a task due to a race between searching 
        // and parking. Crossbeam's Unparker::unpark is a lightweight atomic operation.
        self.worker_pool.notify_many(1);

        if searching == 0 {
            // Only wake the reactor if no one is searching. 
            // We use the 'notifying' flag to coalesce redundant wakeups.
            if !self.notifying.swap(true, Ordering::SeqCst) {
                self.reactor.wakeup();
            }
        } else {
            // Load-based unparking: if the global queue has significantly more tasks
            // than active searchers, wake more workers to assist.
            let queue_len = self.global_queue.len();
            if queue_len > searching {
                // Determine how many extra workers to wake based on queue pressure
                let to_wake = ((queue_len - searching) / 4).max(1).min(4);
                self.worker_pool.notify_many(to_wake);
            }
        }
    }

    pub(crate) fn notify_local(&self) {
        // Light notification for local queue pushes. 
        // If everyone else is parked, they won't steal from this local queue.
        if self.searching_workers.load(Ordering::Acquire) == 0 {
            if !self.notifying.swap(true, Ordering::SeqCst) {
                self.worker_pool.notify_many(1);
                self.reactor.wakeup();
            }
        }
    }

    pub fn incr_searching(&self) {
        self.searching_workers.fetch_add(1, Ordering::SeqCst);
    }

    pub fn decr_searching(&self) {
        self.searching_workers.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn incr_parked(&self) {
        self.parked_workers.fetch_add(1, Ordering::SeqCst);
    }

    pub fn decr_parked(&self) {
        self.parked_workers.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_spawn() {
        let scheduler = Arc::new(Scheduler::new());

        // Initially empty
        assert!(scheduler.steal_global().is_none());

        // Spawn a task
        scheduler.spawn_internal(async { 1 + 1 });

        // Should be in global queue
        assert!(scheduler.steal_global().is_some());
        assert!(scheduler.steal_global().is_none());
    }

    #[test]
    fn test_adaptive_notification() {
        let scheduler = Scheduler::new_with_workers(4);

        // Initially 0
        assert_eq!(scheduler.searching_workers.load(Ordering::Relaxed), 0);
        assert_eq!(scheduler.parked_workers.load(Ordering::Relaxed), 0);

        scheduler.incr_searching();
        assert_eq!(scheduler.searching_workers.load(Ordering::Relaxed), 1);

        scheduler.incr_parked();
        assert_eq!(scheduler.parked_workers.load(Ordering::Relaxed), 1);

        scheduler.decr_searching();
        assert_eq!(scheduler.searching_workers.load(Ordering::Relaxed), 0);

        scheduler.decr_parked();
        assert_eq!(scheduler.parked_workers.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[should_panic(expected = "Spawned on shutdown scheduler")]
    fn test_scheduler_shutdown_panic() {
        let scheduler = Arc::new(Scheduler::new());
        scheduler.shutdown();
        assert!(scheduler.is_shutdown());

        // Should panic
        scheduler.spawn_internal(async { 1 + 1 });
    }
}
