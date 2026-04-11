use std::any::Any;
use std::future::Future;
use std::sync::Arc;
use crate::core::scheduler::scheduler::Scheduler;
use crate::core::runtime::context::Context;

struct Runtime {
    scheduler: Arc<Scheduler>
}

impl Runtime {
    pub fn new(scheduler: Arc<Scheduler>) -> Self {
        Self { scheduler }
    }
    
    pub fn spawn<F, T>(&self, future: F) 
    where
        F: Future<Output = T> + Send + 'static,
        T: Any + Send + 'static,
    {
        self.scheduler.spawn_internal(future);
    }

    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let _guard = Context::enter(self.scheduler.clone());

        let (tx, rx) = std::sync::mpsc::channel();
        self.scheduler.spawn_internal(async move {
            let res = future.await;
            let _ = tx.send(res);
        });
        rx.recv().expect("Runtime internal channel closed")
    }
}