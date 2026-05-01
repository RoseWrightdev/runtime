use std::{future::Future, sync::Arc, sync::Once};
use std::sync::atomic::Ordering;

static INIT_PANIC_HOOK: Once = Once::new();

use num_cpus;
use crossbeam::sync::Parker;
use crate::executor::{Handle, Reactor, Scheduler, Worker, join_handle};

/// The Taiga asynchronous runtime.
/// 
/// The `Runtime` is the top-level container for all the pieces that make 
/// async Rust work. When you create a `Runtime`, it:
/// 
/// 1.  Detects how many CPU cores your computer has.
/// 2.  Creates a [`Scheduler`] to manage tasks.
/// 3.  Creates a [`Reactor`] to handle networking and timers.
/// 4.  Starts several "Worker Threads" (one per core).
/// 
/// Once the `Runtime` is dropped (goes out of scope), it automatically signals 
/// all workers to stop and waits for them to finish, ensuring a clean exit.
pub struct Runtime {
    handle: Handle,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Runtime {
    /// Creates a new runtime with one worker thread per CPU core.
    /// 
    /// This method initializes the multithreaded executor by:
    /// 1. Setting up a panic hook to handle task failures gracefully.
    /// 2. Creating a pool of worker threads, each with its own local LIFO/FIFO queue.
    /// 3. Initializing the [`Reactor`] for non-blocking I/O and timers.
    /// 4. Bootstrapping the [`Scheduler`] with work-stealing capabilities and thread unparkers.
    /// 5. Spawning OS threads to drive the background worker loops.
    pub fn new() -> Self {
        // Ensure the global panic hook is configured only once across all runtime instances.
        // This preserves the default panic behavior while allowing the runtime to 
        // handle worker thread panics uniformly.
        INIT_PANIC_HOOK.call_once(|| {
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                default_hook(info);
            }));
        });

        let num_workers = num_cpus::get();
        let mut local_queues = Vec::with_capacity(num_workers);
        let mut stealers = Vec::with_capacity(num_workers);
        let mut unparkers = Vec::with_capacity(num_workers);
        let mut parkers = Vec::with_capacity(num_workers);

        // Prepare thread-local resources for each worker.
        for _ in 0..num_workers {
            let worker = crossbeam::deque::Worker::new_fifo();
            let parker = Parker::new();
            stealers.push(worker.stealer());
            unparkers.push(parker.unparker().clone());
            local_queues.push(worker);
            parkers.push(parker);
        }

        // Initialize core runtime components.
        let reactor = Arc::new(Reactor::new());
        let reactor_clone = reactor.clone();
        
        let scheduler = Arc::new(Scheduler::new(
            stealers, 
            unparkers,
            Box::new(move || reactor_clone.notify())
        ));
        
        let handle = Handle::new(scheduler, reactor);

        // Spawn OS threads to drive the background worker loops.
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
        }
    }

    /// Spawns a future onto the runtime's execution pool.
    /// 
    /// This method submits a future to the scheduler for background execution. 
    /// It returns a [`join_handle::JoinHandle`], which allows for awaiting the 
    /// task's completion and retrieving its output.
    pub fn spawn<F, T>(&self, future: F) -> join_handle::JoinHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.handle.scheduler.spawn(future)
    }

    /// Runs a future to completion on the current thread, blocking it.
    /// 
    /// While `spawn` runs code in the background, `block_on` waits right here 
    /// until the future is done. This is usually used to "start" your async 
    /// application from a non-async `main` function.
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
        // Signal shutdown.
        // Release ordering ensures that any final state changes made by the 
        // main thread are visible to workers when they see the shutdown signal.
        self.handle.scheduler.shutdown.store(true, Ordering::Release);
        
        // Wake up workers
        for unparker in &self.handle.scheduler.unparkers {
            unparker.unpark();
        }
        
        // Wake up and join reactor.
        // Again, Release ensures workers see the shutdown signal.
        self.handle.reactor.shutdown.store(true, Ordering::Release);
        self.handle.reactor.notify();

        // Join workers
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_new() {
        let runtime = Runtime::new();
        assert!(!runtime.workers.is_empty());
    }

    #[test]
    fn test_runtime_block_on() {
        let runtime = Runtime::new();
        let result = runtime.block_on(async { 42 });
        assert_eq!(result, 42);
    }

    #[test]
    fn test_runtime_spawn() {
        let runtime = Runtime::new();
        let handle = runtime.spawn(async { 100 });
        let result = runtime.block_on(async move { handle.await.unwrap() });
        assert_eq!(result, 100);
    }
}