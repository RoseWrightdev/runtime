use crate::executor::Handle;
use crate::executor::Task;
use crate::executor::Registration;
use crossbeam::deque;
use std::cell::{Cell, RefCell};
use std::sync::Arc;

thread_local! {
    /// The global runtime handle for the current thread.
    pub static CONTEXT: RefCell<Option<Handle>> = const { RefCell::new(None)};
    /// The LIFO slot for the current worker thread.
    pub(crate) static LIFO_SLOT: RefCell<Option<Arc<Task>>> = const { RefCell::new(None) };
    /// Flag to identify if the current thread is a worker thread.
    pub(crate) static IS_WORKER: Cell<bool> = const { Cell::new(false) };
    /// The task currently being executed on this thread.
    pub(crate) static CURRENT_TASK: RefCell<Option<Arc<Task>>> = const { RefCell::new(None) };
    /// The local worker queue for the current thread (RAW POINTER for zero-overhead).
    pub(crate) static LOCAL_QUEUE_PTR: Cell<*mut deque::Worker<Arc<Task>>> = const { Cell::new(std::ptr::null_mut()) };
    /// Thread-local task pools for fast recycling without global contention.
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
/// When created via [`enter`], this guard sets the thread-local [`CONTEXT`] 
/// to the provided [`Handle`]. When dropped, it clears the context.
/// This allows code running on this thread to access the runtime handle
/// implicitly via [`Handle::current`](crate::executor::Handle::current).
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

/// Returns `true` if the current thread is running within a runtime context.
pub(crate) fn is_in_context() -> bool {
    CONTEXT.with(|c| c.borrow().is_some())
}
