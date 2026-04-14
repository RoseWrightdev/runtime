use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::Ordering;
use std::task::Waker;

use crossbeam::deque::{self, Stealer};
use crossbeam::sync::{Parker, Unparker};

use crate::core::executor::context::Context;
use crate::core::executor::local_queue::LocalQueue;
use crate::core::runtime::context::Context as RuntimeContext;
use crate::core::scheduler::task::TaskRef;

#[repr(align(128))]
pub(crate) struct Worker {
    steal_global: fn() -> Option<TaskRef>,
    steal_local: fn(&mut LocalQueue) -> Option<TaskRef>,
    steal_reactor: fn(Option<std::time::Duration>) -> usize,

    index: usize,
    queue: LocalQueue,
    stealer: Stealer<TaskRef>,
    unparker: Unparker,
    tick: usize,
    context: Context,
    pub(crate) parker: Option<Parker>,
}

unsafe impl Send for Worker {}

impl Worker {
    pub fn new(
        index: usize,
        steal_global: fn() -> Option<TaskRef>,
        steal_local: fn(&mut LocalQueue) -> Option<TaskRef>,
        steal_reactor: fn(Option<std::time::Duration>) -> usize,
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
            unparker,
            tick: 0,
            context: {
                let mut ctx = Context::new();
                ctx.task_pool = crate::core::executor::task_pool::Pool::new();
                ctx.worker_index = Some(index);
                ctx.stealers = None;
                ctx.local_queue_ptr = None; // Will be set in run()
                ctx
            },
            parker: Some(parker),
        }
    }

    pub fn get_index(&self) -> usize {
        self.index
    }

    pub fn get_stealer(&self) -> deque::Stealer<TaskRef> {
        self.stealer.clone()
    }

    pub fn get_unparker(&self) -> Unparker {
        self.unparker.clone()
    }

    pub fn get_queue_ptr(&mut self) -> *mut LocalQueue {
        &mut self.queue as *mut _
    }

    pub fn run(&mut self) {
        // Ensure the local_queue_ptr points to our actual queue address
        // which may have changed if we were moved across threads.
        self.context.local_queue_ptr = Some(&mut self.queue as *mut _);

        // Set up the fast-path TLS bridge
        unsafe {
            Context::set_fast_path(&mut self.context as *mut _);
        }

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

            // No work found, attempt to enter searching state
            if let Some(scheduler) = RuntimeContext::current() {
                if let Some(task) = self.search_and_park(&scheduler) {
                    self.execute(task);
                    continue;
                }
            } else {
                // Fallback for non-runtime threads (rare)
                self.park();
            }
        }

        // Clean up bridge
        unsafe {
            Context::set_fast_path(std::ptr::null_mut());
        }
    }

    fn steal(&mut self) -> Option<TaskRef> {
        // 0. check LIFO slot first
        if let Some(task) = self.pop_lifo() {
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

        // 4. drive reactor (non-blocking)
        let _ = (self.steal_reactor)(Some(std::time::Duration::ZERO));

        // 5. steal from workers
        if let Some(task) = (self.steal_local)(&mut self.queue) {
            return Some(task);
        }

        None
    }

    fn execute(&mut self, task: TaskRef) {
        let waker = task.waker();
        let mut cx = std::task::Context::from_waker(&waker);

        // Reset notified flag before poll.
        unsafe {
            let header = task.as_ptr().as_ref();
            header.notified.store(false, Ordering::SeqCst);

            let vtable = header.vtable;

            // Wrap execution in catch_unwind to ensure worker thread survivability
            let result = catch_unwind(AssertUnwindSafe(|| (vtable.poll)(task.as_ptr(), &mut cx)));

            // Production: We don't necessarily handle Poll::Ready here
            // because the future itself is responsible for its own Ready cleanup
            // via the TaskRef Drop (vtable.drop_task).
            if let Err(_) = result {
                header.result_state.store(2, Ordering::SeqCst); // Panicked

                // Wake the joiner if present
                let waker_ptr = header.join_waker.swap(ptr::null_mut(), Ordering::SeqCst);
                if !waker_ptr.is_null() {
                    let waker = Box::from_raw(waker_ptr as *mut Waker);
                    waker.wake();
                }

                eprintln!("Taiga: task panicked on worker {}", self.index);
            }
        }
    }

    fn search_and_park(&mut self, scheduler: &crate::core::scheduler::scheduler::Scheduler) -> Option<TaskRef> {
        scheduler.incr_searching();

        loop {
            if let Some(task) = self.steal() {
                scheduler.decr_searching();
                return Some(task);
            }

            // Atomic handoff: try to decrement. If we were the last searcher, we check I/O.
            if scheduler.searching_workers.fetch_sub(1, Ordering::SeqCst) == 1 {
                if scheduler.reactor.has_wakers() {
                    // Guardian role: block on the reactor to keep I/O alive.
                    let _ = (self.steal_reactor)(Some(std::time::Duration::from_millis(100)));
                    
                    // IMPORTANT: After waking from reactor sleep, we MUST re-increment 
                    // and check for work before deciding whether to park or loop.
                    scheduler.incr_searching();
                    continue;
                }
            }
            
            // Last chance: check global queue after we're technically no longer "searching"
            // but before we actually park.
            if let Some(task) = scheduler.steal_global() {
                scheduler.incr_searching();
                return Some(task);
            }

            break;
        }

        self.park();
        // Note: we don't decr_searching here because it was already decremented by fetch_sub.
        None
    }

    fn park(&mut self) {
        if let Some(parker) = self.parker.as_ref() {
            if let Some(scheduler) = RuntimeContext::current() {
                scheduler.incr_parked();
                parker.park();
                scheduler.decr_parked();
            } else {
                parker.park();
            }
        }
    }

    #[cfg(test)]
    pub fn push_lifo(&mut self, task: TaskRef) {
        if let Some(old_task) = self.context.lifo_slot.replace(task) {
            self.queue.push(old_task);
        }
    }

    pub fn pop_lifo(&mut self) -> Option<TaskRef> {
        self.context.lifo_slot.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scheduler::scheduler::Scheduler;
    use crate::core::scheduler::task::Task;
    use std::sync::Arc;

    #[test]
    fn test_worker_context_isolation() {
        let scheduler = Arc::new(Scheduler::new());
        let mut worker = Worker::new(0, || None, |_| None, |_| 0);
        let task = Task::spawn(async {}, scheduler);

        // This works directly without TLS because Worker owns its context
        worker.push_lifo(task.clone());
        let popped = worker.pop_lifo();

        assert!(popped.is_some());
        assert_eq!(popped.unwrap().as_ptr(), task.as_ptr());
    }

    #[test]
    fn test_worker_parking() {
        use std::time::Duration;
        
        let scheduler = Arc::new(Scheduler::new());
        let mut worker = Worker::new(0, || None, |_| None, |_| 0);
        
        let unparker = worker.get_unparker();
        let scheduler_clone = scheduler.clone();
        let handle = std::thread::spawn(move || {
            let _rt_guard = RuntimeContext::enter(scheduler_clone);
            worker.run();
        });

        // Give worker time to start and (hopefully) park
        std::thread::sleep(Duration::from_millis(100));
        
        // Unpark it
        unparker.unpark();
        
        // Signal shutdown so it actually exits after unpark
        scheduler.shutdown();
        unparker.unpark(); // Secondary unpark to ensure it sees shutdown
        
        // Wait for thread to finish
        let result = handle.join();
        assert!(result.is_ok(), "Worker thread should have exited cleanly after unpark");
    }
}
