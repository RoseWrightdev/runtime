use std::{future::Future, sync::Arc, sync::Once};
use std::sync::atomic::Ordering;

static INIT_PANIC_HOOK: Once = Once::new();

use num_cpus;
use crossbeam::sync::Parker;
use crate::executor::{Handle, Reactor, Scheduler, Worker, join_handle};

pub struct Runtime {
    handle: Handle,
    workers: Vec<std::thread::JoinHandle<()>>,
    reactor_thread: Option<std::thread::JoinHandle<()>>,
}

impl Runtime {
    pub fn new() -> Self {
        INIT_PANIC_HOOK.call_once(|| {
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                if !crate::executor::context::is_in_context() {
                    default_hook(info);
                }
            }));
        });

        let num_workers = num_cpus::get();
        let mut local_queues = Vec::with_capacity(num_workers);
        let mut stealers = Vec::with_capacity(num_workers);
        let mut unparkers = Vec::with_capacity(num_workers);
        let mut parkers = Vec::with_capacity(num_workers);

        for _ in 0..num_workers {
            let worker = crossbeam::deque::Worker::new_fifo();
            let parker = Parker::new();
            stealers.push(worker.stealer());
            unparkers.push(parker.unparker().clone());
            local_queues.push(worker);
            parkers.push(parker);
        }

        let scheduler = Arc::new(Scheduler::new(stealers, unparkers));
        let reactor = Arc::new(Reactor::new());
        let handle = Handle::new(scheduler, reactor.clone());

        // Spawn a dedicated thread for the I/O reactor
        let reactor_handle = reactor.clone();
        let reactor_thread = std::thread::Builder::new()
            .name("reactor".to_string())
            .spawn(move || {
                reactor_handle.run();
            })
            .expect("Failed to spawn reactor thread");

        let mut workers = Vec::with_capacity(num_workers);
        for (i, (queue, parker)) in local_queues.into_iter().zip(parkers).enumerate() {
            let h = handle.clone();
            workers.push(std::thread::spawn(move || {
                let mut worker = Worker::new(i, queue, h, parker);
                worker.run();
            }));
        }

        Self {
            handle,
            workers,
            reactor_thread: Some(reactor_thread),
        }
    }

    pub fn spawn<F, T>(&self, future: F) -> join_handle::JoinHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.handle.scheduler.spawn(future)
    }

    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let _guard = crate::executor::context::enter(self.handle.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.handle.spawn(async move {
            let res = future.await;
            let _ = tx.send(res);
        });
        rx.recv().expect("Runtime internal channel closed")
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Signal shutdown
        self.handle.scheduler.shutdown.store(true, Ordering::Release);
        
        // Wake up workers
        for unparker in &self.handle.scheduler.unparkers {
            unparker.unpark();
        }
        
        // Wake up and join reactor
        self.handle.reactor.shutdown.store(true, Ordering::Release);
        self.handle.reactor.notify();
        
        if let Some(reactor) = self.reactor_thread.take() {
            let _ = reactor.join();
        }

        // Join workers
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}