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
    searching_workers: AtomicUsize,
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
        // Wake up all workers to see the shutdown signal
        self.worker_pool.unpark_all();
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
        let len = self.global_queue.len();
        let searching = self.searching_workers.load(Ordering::Acquire);

        if len > searching {
            let to_wake = (len - searching).min(self.parked_workers.load(Ordering::Acquire));
            if to_wake > 0 {
                self.worker_pool.notify_many(to_wake);
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
}
