use std::cell::RefCell;
use std::sync::Arc;
use crate::core::scheduler::scheduler::Scheduler;

thread_local! {
    static CURRENT_RUNTIME: RefCell<Option<Arc<Scheduler>>> = RefCell::new(None);
}

pub(crate) struct Context {
    prev: Option<Arc<Scheduler>>,
}

impl Context {
    pub fn enter(scheduler: Arc<Scheduler>) -> Self {
        let prev = CURRENT_RUNTIME.with(|rt| {
            let mut current = rt.borrow_mut();
            let prev = current.clone();
            *current = Some(scheduler);
            prev
        });

        Self { prev }
    }
    
    pub fn current() -> Option<Arc<Scheduler>> {
        CURRENT_RUNTIME.with(|rt| rt.borrow().clone())
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        CURRENT_RUNTIME.with(|rt| {
            *rt.borrow_mut() = self.prev.take();
        });
    }
}