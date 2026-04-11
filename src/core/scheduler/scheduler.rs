use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::executor::context::Context as ExecutorContext;

use crate::core::scheduler::global_queue::GlobalQueue;
use crate::core::scheduler::join::JoinHandle;
use crate::core::scheduler::task::Task;
use crate::core::scheduler::worker_pool::Pool;

pub(crate) struct Scheduler {
    pub(crate) global_queue: GlobalQueue,
    worker_pool: Pool,
    shutdown: AtomicBool,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::new_with_workers(num_cpus::get())
    }

    pub fn new_with_workers(num_workers: usize) -> Self {
        Scheduler {
            global_queue: GlobalQueue::new(),
            worker_pool: Pool::new(num_workers),
            shutdown: AtomicBool::new(false),
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

    pub fn steal_global(&self) -> Option<crate::core::scheduler::task::TaskRef> {
        self.global_queue.steal()
    }

    pub fn start(self: &Arc<Self>) {
        self.worker_pool.start(self.clone());
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
