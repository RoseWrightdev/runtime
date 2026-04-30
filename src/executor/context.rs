use crate::executor::Handle;
use crate::executor::Task;
use crate::executor::Registration;
use crossbeam::deque;
use std::cell::{Cell, RefCell};
use std::sync::Arc;

thread_local! {
    /// The global runtime handle for the current thread.
    /// 
    /// This allows you to call `spawn` from anywhere without having to 
    /// pass a "Runtime" object around everywhere.
    pub static CONTEXT: RefCell<Option<Handle>> = const { RefCell::new(None)};

    /// The LIFO slot for the current worker thread.
    /// 
    /// Provides a single-task buffer for immediate re-execution of recently 
    /// woken tasks to maximize CPU cache locality.
    pub(crate) static LIFO_SLOT: RefCell<Option<Arc<Task>>> = const { RefCell::new(None) };

    /// Flag to identify if the current thread is a worker thread.
    pub(crate) static IS_WORKER: Cell<bool> = const { Cell::new(false) };

    /// The task currently being executed on this thread.
    pub(crate) static CURRENT_TASK: RefCell<Option<Arc<Task>>> = const { RefCell::new(None) };

    /// The local worker queue for the current thread.
    /// 
    /// Utilizes a **Raw Pointer** to minimize thread-local access overhead. 
    /// Since workers interact with their local deques frequently, bypassing 
    /// high-level abstractions reduces instruction count in the hot path.
    pub(crate) static LOCAL_QUEUE_PTR: Cell<*mut deque::Worker<Arc<Task>>> = const { Cell::new(std::ptr::null_mut()) };

    /// Thread-local task pools for low-contention recycling.
    /// 
    /// Stores completed `Arc<Task>` objects to allow for zero-allocation 
    /// task spawning. Thread-local pools avoid the synchronization overhead 
    /// of the global scheduler pools.
    pub(crate) static LOCAL_TASK_POOL: [RefCell<Vec<Arc<Task>>>; 11] = const { [
        RefCell::new(Vec::new()), RefCell::new(Vec::new()), RefCell::new(Vec::new()),
        RefCell::new(Vec::new()), RefCell::new(Vec::new()), RefCell::new(Vec::new()),
        RefCell::new(Vec::new()), RefCell::new(Vec::new()), RefCell::new(Vec::new()),
        RefCell::new(Vec::new()), RefCell::new(Vec::new()),
    ]};

    /// Thread-local buffer for batched reactor registrations.
    pub(crate) static REGISTRATION_BUFFER: RefCell<Vec<Registration>> = const { RefCell::new(Vec::new()) };
}

/// A RAII guard that manages the thread-local runtime context.
/// 
/// When you "enter" a runtime context, we set the thread-local variables. 
/// When this guard is dropped, we automatically clear them. This ensures 
/// that one thread's runtime state doesn't "leak" into another.
pub(crate) struct EnterGuard;


impl Drop for EnterGuard {
    fn drop(&mut self) {
        CONTEXT.with(|c| *c.borrow_mut() = None)
    }
}

/// Sets the thread-local runtime context for the current thread.
/// 
/// Returns an [`EnterGuard`] that will clear the context when dropped.
/// This is used by workers and `block_on` to ensure that tasks and futures
/// can access the runtime handle.
pub(crate) fn enter(handle: Handle) -> EnterGuard {
    CONTEXT.with(|c| *c.borrow_mut() = Some(handle));
    EnterGuard
}

/// Marks the current thread as a runtime worker thread.
/// 
/// This enables certain optimizations, such as batched reactor registrations
/// and use of the LIFO task slot.
pub(crate) fn enter_worker() {
    IS_WORKER.with(|w| w.set(true));
}