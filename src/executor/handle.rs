use std::sync::Arc;

use crate::executor::{Reactor, Scheduler, CONTEXT, JoinHandle, Registration, context};

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
        let registration = Registration::IO { key, waker, read, write };

        if context::IS_WORKER.with(|w| w.get()) {
            context::REGISTRATION_BUFFER.with(|buf| {
                let mut b = buf.borrow_mut();
                b.push(registration);
                if b.len() >= 16 {
                    let batch = std::mem::replace(&mut *b, Vec::with_capacity(16));
                    self.reactor.push_batch(batch);
                }
            });
        } else {
            self.reactor.push_batch(vec![registration]);
        }
    }

    pub fn register_timer(&self, deadline: std::time::Instant, waker: std::task::Waker) {
        let registration = Registration::Timer { deadline, waker };

        if context::IS_WORKER.with(|w| w.get()) {
            context::REGISTRATION_BUFFER.with(|buf| {
                let mut b = buf.borrow_mut();
                b.push(registration);
                if b.len() >= 16 {
                    let batch = std::mem::replace(&mut *b, Vec::with_capacity(16));
                    self.reactor.push_batch(batch);
                }
            });
        } else {
            self.reactor.push_batch(vec![registration]);
        }
    }

    /// Flushes any pending registrations from the current worker thread to the reactor.
    pub(crate) fn flush_registrations(&self) {
        if context::IS_WORKER.with(|w| w.get()) {
            context::REGISTRATION_BUFFER.with(|buf| {
                let mut b = buf.borrow_mut();
                if !b.is_empty() {
                    let batch = std::mem::replace(&mut *b, Vec::with_capacity(16));
                    self.reactor.push_batch(batch);
                }
            });
        }
    }
}