use std::cell::{RefCell, Cell};
use crate::executor::Handle;
use std::sync::Arc;
use crate::executor::Task;
use crossbeam::deque;

thread_local! {
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
}

pub(crate) struct EnterGuard;

impl Drop for EnterGuard {
    fn drop(&mut self) {
        CONTEXT.with(|c| *c.borrow_mut() = None)
    }
}

pub(crate) fn enter(handle: Handle) -> EnterGuard {
    CONTEXT.with(|c| *c.borrow_mut() = Some(handle));
    EnterGuard
}

pub(crate) fn enter_worker() {
    IS_WORKER.with(|w| w.set(true));
}

pub(crate) fn is_in_context() -> bool {
    CONTEXT.with(|c| c.borrow().is_some())
}