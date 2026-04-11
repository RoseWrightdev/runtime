use std::cell::RefCell;
use crate::core::executor::task_pool::Pool;

thread_local! {
    static CURRENT_CONTEXT: RefCell<Context> = RefCell::new(Context::new());
}

pub(crate) struct Context {
    pub(crate) task_pool: Pool,
}

impl Context {
    pub fn new() -> Self {
        Self {
            task_pool: Pool::new(),
        }
    }

    pub(crate) fn with<R, F: FnOnce(&mut Context) -> R>(f: F) -> R {
        CURRENT_CONTEXT.with(|ctx| f(&mut ctx.borrow_mut()))
    }
}