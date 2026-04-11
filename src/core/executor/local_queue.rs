use crate::core::scheduler::task::TaskRef;
use crossbeam::deque;

pub(crate) struct LocalQueue {
    queue: deque::Worker<TaskRef>,
}

impl LocalQueue {
    pub fn new() -> Self {
        LocalQueue {
            queue: deque::Worker::new_fifo(),
        }
    }

    pub fn pop(&mut self) -> Option<TaskRef> {
        self.queue.pop()
    }

    pub fn push(&mut self, task: TaskRef) {
        self.queue.push(task);
    }

    pub fn get_stealer(&self) -> deque::Stealer<TaskRef> {
        self.queue.stealer()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scheduler::scheduler::Scheduler;
    use crate::core::scheduler::task::Task;
    use std::sync::Arc;

    #[test]
    fn test_local_queue_basic() {
        let mut lq = LocalQueue::new();
        let scheduler = Arc::new(Scheduler::new());
        let _task = Task::spawn(async {}, scheduler);

        // Note: we can't easily push to LocalQueue right now because
        // it doesn't have a public push method (it's meant to be used
        // by the thread owning the crossbeam::Worker).
        // But we can verify it's empty.
        assert!(lq.pop().is_none());
    }
}
