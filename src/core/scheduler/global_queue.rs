use crate::core::scheduler::task::TaskRef;
use crossbeam::deque::{Injector, Steal};

pub(crate) struct GlobalQueue {
    queue: Injector<TaskRef>,
}

impl GlobalQueue {
    pub fn new() -> Self {
        GlobalQueue {
            queue: Injector::new(),
        }
    }

    pub fn push(&self, task: TaskRef) {
        self.queue.push(task);
    }

    pub fn steal(&self) -> Option<TaskRef> {
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
    use std::sync::Arc;

    #[test]
    fn test_global_queue_push_steal() {
        let gq = GlobalQueue::new();
        let scheduler = Arc::new(Scheduler::new());
        let task = Task::spawn(async {}, scheduler);

        gq.push(task.clone());
        let stolen = gq.steal().expect("Task should be present");
        assert_eq!(task.as_ptr(), stolen.as_ptr());
        assert!(gq.steal().is_none());
    }
}
