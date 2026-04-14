use crate::core::executor::local_queue::LocalQueue;
use crate::core::executor::task_pool::Pool;
use crate::core::scheduler::scheduler::Scheduler;
use crate::core::scheduler::task::TaskRef;

use std::any::Any;
use std::cell::RefCell;
use std::sync::Arc;

use std::cell::Cell;
use std::ptr;

use crossbeam::utils::CachePadded;
thread_local! {
    static SLOW_CONTEXT: RefCell<Box<Context>> = RefCell::new(Box::new(Context::new()));
    static FAST_CONTEXT: Cell<*mut Context> = Cell::new(ptr::null_mut());
}

pub(crate) struct Context {
    pub(crate) task_pool: Pool,
    pub(crate) worker_index: Option<usize>,
    pub(crate) stealers: Option<Arc<dyn Any + Send + Sync>>,
    pub(crate) local_queue_ptr: Option<*mut LocalQueue>,

    // Isolated LIFO slot using standard CachePadded instead of manual padding
    pub(crate) lifo_slot: CachePadded<Option<TaskRef>>,
}

unsafe impl Send for Context {}

impl Context {
    pub fn new() -> Self {
        Self {
            task_pool: Pool::new(),
            worker_index: None,
            stealers: None,
            lifo_slot: CachePadded::new(None),
            local_queue_ptr: None,
        }
    }

    pub(crate) fn try_push_local(task: TaskRef, scheduler: &Scheduler) -> bool {
        Self::with(|ctx| {
            if let Some(queue_ptr) = ctx.local_queue_ptr {
                // If LIFO slot is full, move the OLD task to the local queue
                // and put the NEW task in the LIFO slot.
                if let Some(old_task) = ctx.lifo_slot.replace(task) {
                    unsafe {
                        (*queue_ptr).push(old_task);
                        // Notify that a task is available for stealing
                        scheduler.notify_local();
                    }
                }
                true
            } else {
                false
            }
        })
    }

    pub(crate) unsafe fn set_fast_path(ptr: *mut Context) {
        FAST_CONTEXT.with(|ctx| ctx.set(ptr));
    }

    pub(crate) fn with<R, F: FnOnce(&mut Context) -> R>(f: F) -> R {
        let fast_ptr = FAST_CONTEXT.with(|ctx| ctx.get());
        if !fast_ptr.is_null() {
            unsafe { f(&mut *fast_ptr) }
        } else {
            SLOW_CONTEXT.with(|ctx| f(&mut **ctx.borrow_mut()))
        }
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

    #[test]
    fn test_try_push_local_logic() {
        use crate::core::scheduler::scheduler::Scheduler;
        use crate::core::scheduler::task::Task;

        let scheduler = Arc::new(Scheduler::new_with_workers(1));
        let mut local_queue = LocalQueue::new();

        // 1. Manually set up the local queue pointer in the context
        Context::with(|ctx| {
            ctx.local_queue_ptr = Some(&mut local_queue as *mut _);
        });

        // Use Task::spawn (pub(crate)) to create test task references
        let t1 = Task::spawn(async {}, scheduler.clone());
        let t2 = Task::spawn(async {}, scheduler.clone());
        let t3 = Task::spawn(async {}, scheduler.clone());

        // 2. First push: Should go to the empty LIFO slot
        assert!(Context::try_push_local(t1.clone(), &scheduler));
        // LIFO slot is private so we check the queue instead
        assert!(
            local_queue.pop().is_none(),
            "t1 should be in LIFO, not in queue"
        );

        // 3. Second push: Should move t1 to the local queue and put t2 in LIFO
        assert!(Context::try_push_local(t2.clone(), &scheduler));
        assert!(
            local_queue.pop().is_some(),
            "t1 should have moved to the queue"
        );
        assert!(
            local_queue.pop().is_none(),
            "Only one task (t1) should be in the queue"
        );

        // 4. Third push: Should move t2 to the local queue and put t3 in LIFO
        assert!(Context::try_push_local(t3.clone(), &scheduler));
        assert!(
            local_queue.pop().is_some(),
            "t2 should have moved to the queue"
        );
        assert!(
            local_queue.pop().is_none(),
            "Only one task (t2) should be in the queue"
        );

        // Clean up to avoid dangling pointers in TLS
        Context::with(|ctx| {
            ctx.local_queue_ptr = None;
            ctx.lifo_slot.take();
        });
    }
}
