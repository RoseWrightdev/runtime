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
}