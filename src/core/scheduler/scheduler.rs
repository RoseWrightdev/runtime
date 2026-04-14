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
        if !ExecutorContext::try_push_local(task.clone()) {
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

    pub(crate) fn notify(&self) {
        if self.searching_workers.load(Ordering::Acquire) == 0 {
            self.worker_pool.notify_many(1);
        }
        // Always wake the reactor to be safe, especially if the last searching
        // worker is currently blocked in mio::poll().
        self.reactor.wakeup();
    }

    pub fn notify_adaptive(&self) {
        let searching = self.searching_workers.load(Ordering::Acquire);

        // If searching workers are low, we must unpark one.
        // We also always wake the reactor to pull the Guardian out of poll().
        if searching == 0 {
            self.notify();
        } else {
            // Even with searchers, we unpark an extra one occasionally under load
            // to mitigate "park-at-the-same-moment" races.
            if self.global_queue.len() > searching {
                self.worker_pool.notify_many(1);
            }
            self.reactor.wakeup();
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
