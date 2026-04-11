use std::any::Any;
use std::future::Future;
use std::sync::Arc;
use crate::core::scheduler::scheduler::Scheduler;

struct Runtime<'a> {
    scheduler: &'a Scheduler
}

impl<'a> Runtime<'a> {
    pub fn new(scheduler: &'a Scheduler) -> Self {
        Self { scheduler }
    }
    
    pub fn spawn<F, T>(self: &Arc<Self>, future: F) 
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

        let (tx, rx) = std::sync::mpsc::channel();
        self.scheduler.spawn_internal(async move {
            let res = future.await;
            let _ = tx.send(res);
        });
        rx.recv().expect("Runtime internal channel closed")
    }
}