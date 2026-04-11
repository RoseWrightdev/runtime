use crate::core::executor::local_queue::LocalQueue;
use crate::core::executor::task_pool::Pool;
use crate::core::scheduler::task::TaskRef;
use std::any::Any;
use std::cell::RefCell;
use std::sync::Arc;

thread_local! {
    static CURRENT_CONTEXT: RefCell<Context> = RefCell::new(Context::new());
}

pub(crate) struct Context {
    pub(crate) task_pool: Pool,
    pub(crate) worker_index: Option<usize>,
    pub(crate) stealers: Option<Arc<dyn Any + Send + Sync>>,

    // LIFO slot for the current worker
    pub(crate) lifo_slot: Option<TaskRef>,
    // Pointer to the local queue, safe because it's only accessed on the same thread
    pub(crate) local_queue_ptr: Option<*mut LocalQueue>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            task_pool: Pool::new(),
            worker_index: None,
            stealers: None,
            lifo_slot: None,
            local_queue_ptr: None,
        }
    }

    pub(crate) fn try_push_local(task: TaskRef) -> bool {
        Self::with(|ctx| {
            if let Some(queue_ptr) = ctx.local_queue_ptr {
                // If LIFO slot is full, move the OLD task to the local queue
                // and put the NEW task in the LIFO slot.
                if let Some(old_task) = ctx.lifo_slot.replace(task) {
                    unsafe {
                        (*queue_ptr).push(old_task);
                    }
                }
                true
            } else {
                false
            }
        })
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
            let stealers = ctx
                .stealers
                .as_ref()
                .unwrap()
                .downcast_ref::<Vec<i32>>()
                .unwrap();
            assert_eq!(stealers.len(), 3);
            assert_eq!(stealers[0], 1);
        });
    }
}
