use std::sync::Arc;

use crossbeam::deque;

use crate::core::scheduler::task::Task;

pub(crate) struct LocalQueue {
    queue: deque::Worker<Arc<Task>>
}

impl LocalQueue {
    pub fn new() -> Self {
        LocalQueue {
            queue: deque::Worker::new_fifo(),
        }
    }
    
    pub fn pop(&mut self) -> Option<Arc<Task>> {
        self.queue.pop()
    }

    pub fn get_stealer(&self) -> deque::Stealer<Arc<Task>> {
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
        let task = Task::new(async {}, scheduler);
        
        // Note: we can't easily push to LocalQueue right now because 
        // it doesn't have a public push method (it's meant to be used
        // by the thread owning the crossbeam::Worker).
        // But we can verify it's empty.
        assert!(lq.pop().is_none());
    }
}