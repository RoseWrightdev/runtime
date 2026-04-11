use std::sync::Arc;
use crossbeam::deque::{Injector, Steal};
use crate::core::task::Task;

pub(crate) struct GlobalQueue {
    queue: Injector<Arc<Task>>,
}

impl GlobalQueue {
    pub fn new() -> Self {
        GlobalQueue {
            queue: Injector::new(),
        }
    }
    
    pub fn push(&self, task: Arc<Task>) {
        self.queue.push(task);
    }

    pub fn steal(&self) -> Option<Arc<Task>> {
        loop {
            match self.queue.steal() {
                Steal::Success(task) => return Some(task),
                Steal::Retry => continue,
                Steal::Empty => return None,
            }
        }
    }
}