use std::sync::Arc;

use crate::executor::{Reactor, Scheduler, CONTEXT, JoinHandle};

#[derive(Clone)]
pub struct Handle {
    pub scheduler: Arc<Scheduler>,
    pub reactor: Arc<Reactor>,
}

impl Handle {
    pub(crate) fn new(scheduler: Arc<Scheduler>, reactor: Arc<Reactor>) -> Self {
        Handle { scheduler, reactor }
    }

    pub fn current() -> Self {
        CONTEXT.with(|c| c.borrow().as_ref().expect("Not in a runtime context").clone())
    }

    pub fn spawn<F, T>(&self, future: F) -> JoinHandle<T>
    where 
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.scheduler.spawn(future)
    }

    pub fn register_io(&self, key: usize, waker: std::task::Waker, read: bool, write: bool) {
        self.reactor.register(key, waker, read, write);
    }
}