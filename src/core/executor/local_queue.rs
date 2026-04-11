use crate::core::scheduler::task::TaskRef;
use crossbeam::deque::{self, Stealer};

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

    pub fn steal_into(&mut self, stealer: &Stealer<TaskRef>) -> Option<TaskRef> {
        loop {
            match stealer.steal_batch_and_pop(&self.queue) {
                deque::Steal::Success(task) => return Some(task),
                deque::Steal::Retry => continue,
                deque::Steal::Empty => return None,
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

    #[test]
    fn test_local_queue_batch_steal() {
        let mut q_src = LocalQueue::new();
        let mut q_dest = LocalQueue::new();
        let scheduler = Arc::new(Scheduler::new());

        // Push 10 tasks into the source queue
        for _ in 0..10 {
            q_src.push(Task::spawn(async {}, scheduler.clone()));
        }

        let stealer = q_src.get_stealer();

        // This method doesn't exist yet - strict TDD Red phase
        let stolen_task = q_dest.steal_into(&stealer);

        assert!(
            stolen_task.is_some(),
            "Should have stolen at least one task for immediate execution"
        );

        // crossbeam's steal_batch_and_pop usually steals half
        // Source had 10. Dest should now have stolen half (5).
        // One is returned as stolen_task, so q_dest should have 4 remaining.
        let mut dest_count = 0;
        while q_dest.pop().is_some() {
            dest_count += 1;
        }

        assert_eq!(
            dest_count, 4,
            "Should have 4 tasks left in dest queue (half size - 1)"
        );

        // Source should also have around 5 left
        let mut src_count = 0;
        while q_src.pop().is_some() {
            src_count += 1;
        }
        assert_eq!(src_count, 5, "Source should have 5 tasks left (half size)");
    }
}
