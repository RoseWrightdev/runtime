use std::cell::RefCell;
use std::any::Any;
use std::sync::Arc;
use crate::core::executor::task_pool::Pool;

thread_local! {
    static CURRENT_CONTEXT: RefCell<Context> = RefCell::new(Context::new());
}

pub(crate) struct Context {
    pub(crate) task_pool: Pool,
    pub(crate) worker_index: Option<usize>,
    pub(crate) stealers: Option<Arc<dyn Any + Send + Sync>>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            task_pool: Pool::new(),
            worker_index: None,
            stealers: None,
        }
    }

    pub(crate) fn with<R, F: FnOnce(&mut Context) -> R>(f: F) -> R {
        CURRENT_CONTEXT.with(|ctx| f(&mut ctx.borrow_mut()))
    }
}