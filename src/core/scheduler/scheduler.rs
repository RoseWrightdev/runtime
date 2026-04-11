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

    pub fn spawn_internal<F, T>(self: &Arc<Self>, future: F)
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        if !self.shutdown {
            let scheduler = self.clone();
            let task = Task::new(async move {
                let _ = future.await;
            }, scheduler);
            self.global_queue.push(task);
        }
    }

    pub fn steal_global(&self) -> Option<Arc<Task>> {
        self.global_queue.steal()
    }
}
