use crate::core::scheduler::scheduler::Scheduler;
use std::cell::RefCell;
use std::sync::Arc;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_nesting() {
        let scheduler1 = Arc::new(Scheduler::new());
        let scheduler2 = Arc::new(Scheduler::new());

        assert!(Context::current().is_none());

        {
            let _guard1 = Context::enter(scheduler1.clone());
            assert!(Arc::ptr_eq(&Context::current().unwrap(), &scheduler1));

            {
                let _guard2 = Context::enter(scheduler2.clone());
                assert!(Arc::ptr_eq(&Context::current().unwrap(), &scheduler2));
            }

            // Back to scheduler 1 after guard 2 drops
            assert!(Arc::ptr_eq(&Context::current().unwrap(), &scheduler1));
        }

        // Back to none after guard 1 drops
        assert!(Context::current().is_none());
    }
}
