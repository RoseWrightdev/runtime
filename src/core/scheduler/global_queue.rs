use std::sync::Arc;
use crossbeam::deque::{Injector, Steal};
use crate::core::scheduler::task::Task;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scheduler::scheduler::Scheduler;
    use crate::core::scheduler::task::Task;

    #[test]
    fn test_global_queue_push_steal() {
        let gq = GlobalQueue::new();
        let scheduler = Arc::new(Scheduler::new());
        let task = Task::new(async {}, scheduler);
        
        gq.push(task.clone());
        let stolen = gq.steal().expect("Task should be present");
        assert!(Arc::ptr_eq(&task, &stolen));
        assert!(gq.steal().is_none());
    }
}