use crate::core::runtime::context::Context;
use crate::core::scheduler::join::JoinHandle;
use crate::core::scheduler::scheduler::Scheduler;
use std::any::Any;
use std::future::Future;
use std::sync::Arc;

pub struct Runtime {
    scheduler: Arc<Scheduler>,
}

impl Runtime {
    pub fn new() -> Self {
        Self::with_workers(num_cpus::get())
    }

    pub fn with_workers(num_workers: usize) -> Self {
        let scheduler = Arc::new(Scheduler::new_with_workers(num_workers));
        scheduler.start();
        Self { scheduler }
    }

    pub fn spawn<F, T>(&self, future: F) -> JoinHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Any + Send + 'static,
    {
        self.scheduler.spawn_internal(future)
    }

    pub fn shutdown(&self) {
        self.scheduler.shutdown();
    }

    pub fn is_shutdown(&self) -> bool {
        self.scheduler.is_shutdown()
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

    pub fn join(&self) {
        self.scheduler.join();
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown();
        self.join();
    }
}
