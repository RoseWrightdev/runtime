use crate::core::scheduler::task::TaskRef;
use crossbeam::deque::{Injector, Steal};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct GlobalQueue {
    queue: Injector<TaskRef>,
    len: AtomicUsize,
}

impl GlobalQueue {
    pub fn new() -> Self {
        GlobalQueue {
            queue: Injector::new(),
            len: AtomicUsize::new(0),
        }
    }

    pub fn push(&self, task: TaskRef) {
        self.queue.push(task);
        self.len.fetch_add(1, Ordering::SeqCst);
    }

    pub fn steal(&self) -> Option<TaskRef> {
        loop {
            match self.queue.steal() {
                Steal::Success(task) => {
                    self.len.fetch_sub(1, Ordering::SeqCst);
                    return Some(task);
                }
                Steal::Retry => continue,
                Steal::Empty => return None,
            }
        }
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
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
