use std::future::Future;
use std::sync::Arc;

use crate::core::scheduler::global_queue::GlobalQueue;
use crate::core::scheduler::worker_pool::Pool;
use crate::core::scheduler::task::Task;

pub(crate) struct Scheduler {
    global_queue: GlobalQueue,
    worker_pool: Pool,
    shutdown: bool,
}

impl Scheduler {
    pub fn new() -> Self {
        let num_workers = num_cpus::get();

        Scheduler {
            global_queue: GlobalQueue::new(),
            worker_pool: Pool::new(num_workers),
            shutdown: false,
        }
    }

    pub fn spawn_internal<F, T>(&self, future: F)
    where
        F: Future<Output = T> + Send + 'static,
    {
        if !self.shutdown {
            unimplemented!();
            let _ = future; // Suppress unused future warning for now
            let task = Arc::new(Task::new());
            self.global_queue.push(task);
        }
    }

    pub fn steal_global(&self) -> Option<Arc<Task>> {
        self.global_queue.steal()
    }
}
