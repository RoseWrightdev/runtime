use std::sync::Arc;

use crate::executor::{Reactor, Scheduler, CONTEXT, JoinHandle, Registration, context};

/// A handle to the Taiga runtime.
/// 
/// The `Handle` provides access to the [`Scheduler`] for spawning tasks and the 
/// [`Reactor`] for registering I/O and timers. It can be cloned cheaply.
#[derive(Clone)]
pub struct Handle {
    pub scheduler: Arc<Scheduler>,
    pub reactor: Arc<Reactor>,
}

impl Handle {
    pub(crate) fn new(scheduler: Arc<Scheduler>, reactor: Arc<Reactor>) -> Self {
        Handle { scheduler, reactor }
    }

    /// Returns the handle for the runtime associated with the current thread.
    /// 
    /// # Panics
    /// 
    /// Panics if the current thread is not running within a runtime context.
    pub fn current() -> Self {
        CONTEXT.with(|c| c.borrow().as_ref().expect("Not in a runtime context").clone())
    }

    /// Spawns a future onto the runtime.
    /// 
    /// Returns a [`JoinHandle`] that can be used to await the result.
    pub fn spawn<F, T>(&self, future: F) -> JoinHandle<T>
    where 
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.scheduler.spawn(future)
    }

    /// Registers an I/O event with the reactor.
    /// 
    /// If called from a worker thread, the registration is buffered and sent to the 
    /// reactor in batches (every 16 registrations) to reduce cross-thread contention.
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

    /// Registers a timer with the reactor.
    /// 
    /// Similar to [`Self::register_io`], registrations from worker threads are buffered 
    /// to improve performance.
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
    /// 
    /// This should be called by workers before they go to sleep to ensure all 
    /// pending events are submitted for polling.
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