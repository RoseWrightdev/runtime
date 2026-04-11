use std::sync::Arc;
use std::task::Poll;
use std::panic::{catch_unwind, AssertUnwindSafe};
use crate::core::runtime::context::Context as RuntimeContext;
use crate::core::executor::context::Context as ExecutorContext;

use crossbeam::deque::{self, Stealer};
use crossbeam::sync::{Parker, Unparker};

use crate::core::executor::local_queue::LocalQueue;
use crate::core::scheduler::task::Task;

pub(crate) struct Worker {
    steal_global: fn() -> Option<Arc<Task>>,
    steal_local: fn() -> Option<Arc<Task>>,
    steal_reactor: fn() -> (),

    index: usize,
    queue: LocalQueue,
    stealer: Stealer<Arc<Task>>,
    parker: Parker,
    unparker: Unparker,
    tick: usize,
}

impl Worker {
    pub fn new(
        index: usize,
        steal_global: fn() -> Option<Arc<Task>>,
        steal_local: fn() -> Option<Arc<Task>>,
        steal_reactor: fn() -> (),
    ) -> Self {
        let queue = LocalQueue::new();
        let stealer = queue.get_stealer();
        let parker = Parker::new();
        let unparker = parker.unparker().clone();

        Self {
            steal_global,
            steal_local,
            steal_reactor,

            index,
            queue,
            stealer,
            parker,
            unparker,
            tick: 0,
        }
    }

    pub fn get_index(&self) -> usize {
        self.index
    }

    pub fn get_stealer(&self) -> deque::Stealer<Arc<Task>> {
        self.stealer.clone()
    }

    pub fn get_unparker(&self) -> Unparker {
        self.unparker.clone()
    }

    pub fn get_queue_ptr(&mut self) -> *mut LocalQueue {
        &mut self.queue as *mut _
    }

    pub fn run(&mut self) {
        loop {
            // Check for shutdown signal from global scheduler
            if let Some(scheduler) = RuntimeContext::current() {
                if scheduler.is_shutdown() {
                    break;
                }
            }

            if let Some(task) = self.steal() {
                self.execute(task);
                continue;
            }

            self.park()
        }
    }
    fn steal(&mut self) -> Option<Arc<Task>> {
        // 0. check LIFO slot first
        let task = ExecutorContext::with(|ctx| ctx.lifo_slot.take());
        if let Some(task) = task {
            return Some(task);
        }

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

        // 4. drive reactor
        (self.steal_reactor)();

        // 5. steal from workers
        if let Some(task) = (self.steal_local)() {
            return Some(task);
        }

        None
    }

    fn execute(&mut self, task: Arc<Task>) {
        let waker = futures::task::waker_ref(&task);
        let mut cx = std::task::Context::from_waker(&waker);

        let future_ptr = task.future.get();
        
        // Wrap execution in catch_unwind to ensure worker thread survivability
        let result = catch_unwind(AssertUnwindSafe(|| {
            unsafe { (*future_ptr).poll(&mut cx) }
        }));

        match result {
            Ok(Poll::Ready(_)) => {}
            Ok(Poll::Pending) => {}
            Err(_) => {
                // Task panicked. 
                // In a production runtime, we'd log this or propagate to a join handle.
                eprintln!("Taiga: task panicked on worker {}", self.index);
            }
        }
    }

    fn park(&mut self) {}
}
