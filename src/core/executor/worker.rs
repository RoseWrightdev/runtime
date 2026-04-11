use std::sync::Arc;

use crossbeam::deque::{self, Stealer};
use crossbeam::sync::{Parker, Unparker};

use crate::core::executor::local_queue::LocalQueue;
use crate::core::executor::task_pool;
use crate::core::scheduler::task::Task;
pub(crate) struct Worker {
    steal_global: fn() -> Option<Arc<Task>>,
    steal_local: fn() -> Option<Arc<Task>>,
    drive_reactor: fn() -> (),

    index: usize,
    queue: LocalQueue,
    pool: task_pool::Pool,
    tick: usize,
}

impl Worker {
    pub fn new(
        index: usize,
        steal_global: fn() -> Option<Arc<Task>>,
        steal_local: fn() -> Option<Arc<Task>>,
        steal_reactor: fn() -> unimplemented!(),

        stealer: Stealer<Arc<Task>>,
        parker: Parker,
        unparker: Unparker,
    ) -> Self {
        unimplemented!()
    }

    pub fn get_stealer(&self) -> deque::Stealer<Arc<Task>> {
        unimplemented!()
    }

    pub fn get_parker(&self) -> Parker {
        unimplemented!()
    }

    pub fn get_unparker(&self) -> Unparker {
        unimplemented!()
    }

    pub fn run(&mut self) {
        loop {
            if let Some(task) = self.steal() {
                self.execute(task);
                continue;
            }

            self.park()
        }
    }

    fn steal(&mut self) -> Option<Arc<Task>> {
        self.tick = self.tick.wrapping_add(1);

        // 1. check global queue first to prevent starvation
        // 61 is a prime number used to avoid synchronized patterns
        if self.tick % 61 == 0 {
            // steal_global locks the global queue and pops a task
            if let Some(task) = (self.steal_global)() {
                return Some(task);
            }
        }

        // 2. Pop from local queue
        if let Some(task) = self.queue.pop() {
            return Some(task);
        }

        // 3. If local is empty, check global queue (if we didn't already)
        if self.tick % 61 != 0 {
            if let Some(task) = (self.steal_global)() {
                return Some(task);
            }
        }

        // 4. steal from workers
        if let Some(task) = (self.steal_local)() {
            return Some(task);
        }

        None
    }

    fn execute(&mut self, task: Arc<Task>) {
        // Context is a wrapper around the Waker. 
        // It's what gets passed into poll() so the future can
        // register itself to be woken later.
        let waker = futures::task::waker_ref(&task);
        let mut cx = std::task::Context::from_waker(&waker);
        let mut future = task.future.lock().unwrap();

        // future executes here, up until completion
        // or the next .await point
        match future.as_mut().poll(&mut cx) {
            // Future completed.
            Poll::Ready(_) => {}

            // Future is waiting on something (I/O, timer).
            // It has already registered its waker with whatever
            // will wake it, so we just leave it alone until
            // wake() re-queues it.
            Poll::Pending => {}
        }

    }

    fn park(&mut self) {}
}
