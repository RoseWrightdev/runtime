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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_context_metadata() {
        Context::with(|ctx| {
            ctx.worker_index = Some(5);
            ctx.stealers = Some(Arc::new(vec![1, 2, 3]));
        });

        Context::with(|ctx| {
            assert_eq!(ctx.worker_index, Some(5));
            let stealers = ctx.stealers.as_ref()
                .unwrap()
                .downcast_ref::<Vec<i32>>()
                .unwrap();
            assert_eq!(stealers.len(), 3);
            assert_eq!(stealers[0], 1);
        });
    }
}