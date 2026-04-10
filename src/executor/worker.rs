use std::{
    sync::{atomic::Ordering, Arc},
    task::Context,
};

use crate::executor::context;
use crossbeam::deque;
use crossbeam::sync::Parker;
use crossbeam_deque::Steal;
use futures::task::waker_ref;

use crate::executor::{
    task::{STATE_IDLE, STATE_POLLING, STATE_SCHEDULED},
    Handle, Scheduler, Task,
};

pub struct Worker {
    id: usize,
    queue: Option<deque::Worker<Arc<Task>>>,
    handle: Handle,
    tick: usize,
    budget: u8,
    parker: Parker,
    rng: u32,
}

#[inline(always)]
fn prefetch<T>(ptr: *const T) {
    unsafe {
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("prfm pldl1keep, [{}]", in(reg) ptr, options(nostack, preserves_flags, nomem));
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        core::arch::asm!("prefetcht0 [{}]", in(reg) ptr, options(nostack, preserves_flags, nomem));
    }
}

impl Worker {
    pub fn new(id: usize, queue: deque::Worker<Arc<Task>>, handle: Handle, parker: Parker) -> Self {
        Self {
            id,
            queue: Some(queue),
            handle,
            tick: 0,
            budget: 128,
            parker,
            rng: (id as u32).wrapping_add(1),
        }
    }

    pub fn run(&mut self) {
        let _guard = context::enter(self.handle.clone());
        // Register this thread as a worker to safely use LIFO slot
        context::enter_worker();

        // Use RAW POINTER for zero-overhead local queue access
        let mut queue = self.queue.take().expect("Worker started without a queue");
        context::LOCAL_QUEUE_PTR.with(|q| q.set(&mut queue as *mut _));

        loop {
            if self.handle.scheduler.shutdown.load(Ordering::Acquire) {
                break;
            }

            self.cooperative_yield(&mut queue);

            if let Some(task) = self.next_local_task(&mut queue) {
                self.execute(task);
                continue;
            }

            self.search_and_park();
        }

        // Cleanup raw pointer on exit and restore the queue
        context::LOCAL_QUEUE_PTR.with(|q| q.set(std::ptr::null_mut()));
        self.queue = Some(queue);
    }

    fn cooperative_yield(&mut self, queue: &mut deque::Worker<Arc<Task>>) {
        // If the budget is exhausted, move a task to the global injector to ensure other
        // workers have a chance to pick it up, preventing this worker from hogging tasks.
        if self.budget == 0 {
            self.budget = 128;
            let task = context::LIFO_SLOT
                .with(|slot| slot.borrow_mut().take())
                .or_else(|| queue.pop());

            if let Some(task) = task {
                self.handle.scheduler.inject(task);
            }
        }
    }

    fn next_local_task(&mut self, queue: &mut deque::Worker<Arc<Task>>) -> Option<Arc<Task>> {
        // 1. Check LIFO slot for highest priority task (woken by the same thread)
        let task = context::LIFO_SLOT.with(|slot| slot.borrow_mut().take());
        if task.is_some() {
            return task;
        }

        // 2. Pop from local queue
        if let Some(task) = queue.pop() {
            return Some(task);
        }

        // 3. Periodically steal from global queue to ensure fairness
        if self.tick % 61 == 0 {
            if let Steal::Success(task) = self.handle.scheduler.steal() {
                return Some(task);
            }
        }

        None
    }

    fn search_and_park(&mut self) {
        // 4. Steal from other workers or global queue if local is empty        
        self.handle.scheduler.searching_workers.fetch_add(1, Ordering::Relaxed);
        if let Some(task) = self.steal() {
            prefetch(Arc::as_ptr(&task));
            self.handle
                .scheduler
                .searching_workers
                .fetch_sub(1, Ordering::Relaxed);
            self.execute(task);
            return;
        }

        // If still no work, try a short spin-loop before parking.
        for _ in 0..150 {
            if let Some(task) = std::hint::black_box(self.steal()) {
                prefetch(Arc::as_ptr(&task));
                self.handle
                    .scheduler
                    .searching_workers
                    .fetch_sub(1, Ordering::Relaxed);
                self.execute(task);
                return;
            }
            if let Steal::Success(task) = std::hint::black_box(self.handle.scheduler.steal()) {
                prefetch(Arc::as_ptr(&task));
                self.handle
                    .scheduler
                    .searching_workers
                    .fetch_sub(1, Ordering::Relaxed);
                self.execute(task);
                return;
            }
            std::hint::spin_loop();
        }

        // 4. Fallback to parking or reactor polling
        self.handle.scheduler.searching_workers.fetch_sub(1, Ordering::Relaxed);

        // One final attempt to steal from the global injector if searching count is low.
        // This mitigates the "last searcher" race where a task is injected just as we park.
        if self
            .handle
            .scheduler
            .searching_workers
            .load(Ordering::Acquire)
            < std::cmp::max(2, num_cpus::get() / 2)
        {
            if let Steal::Success(task) = self.handle.scheduler.steal() {
                prefetch(Arc::as_ptr(&task));
                self.execute(task);
                return;
            }
        }

        self.handle.scheduler.sleeping_workers.fetch_add(1, Ordering::Release);

        // Sync any pending reactor registrations from TLS before waiting
        self.handle.flush_registrations();

        // Try to drive the reactor. If we acquire the lock, we wait for I/O events.
        // If another worker is already driving it, we simply park.
        if !self.handle.reactor.try_poll() {
            self.handle.flush_registrations();
            self.parker.park();
        }

        self.handle
            .scheduler
            .sleeping_workers
            .fetch_sub(1, Ordering::Release);
    }

    fn execute(&mut self, task: Arc<Task>) {
        if task
            .exec_state
            .state
            .compare_exchange(
                STATE_SCHEDULED,
                STATE_POLLING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            // Create a Waker from the task so the future can wake itself up when ready.
            // The waker_ref function creates a Waker that, when called, will schedule this task for execution.
            let waker = waker_ref(&task);
            let mut cx = Context::from_waker(&waker);

            // Transition from SCHEDULED to POLLING.
            let future = unsafe { &mut *task.future.get() };

            // Decrement budget upon execution
            self.budget = self.budget.saturating_sub(1);

            // Register this as the current task to allow self-referencing (for JoinHandle result writing)
            context::CURRENT_TASK.with(|c| *c.borrow_mut() = Some(task.clone()));

            let poll_result = unsafe { (future.poll_fn)(future.ptr, &mut cx) };

            context::CURRENT_TASK.with(|c| *c.borrow_mut() = None);

            if let std::task::Poll::Ready(_) = poll_result {
                unsafe {
                    (future.drop_fn)(future.ptr);
                    future.drop_fn = |_| {};
                    future.poll_fn = |_, _| std::task::Poll::Ready(());
                }

                self.recycle_task(task);
            } else {
                // Return to IDLE, unless a wake occurred (state is now SCHEDULED)
                if task
                    .exec_state
                    .state
                    .compare_exchange(
                        STATE_POLLING,
                        STATE_IDLE,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    // It was woken during poll, must re-schedule
                    self.handle.scheduler.inject(task);
                }
            }
        }

        self.tick += 1;
    }

    fn recycle_task(&self, task: Arc<Task>) {
        // Recycle task via Thread-Local Pool first (ZERO contention)
        if Arc::strong_count(&task) == 1 {
            let layout = unsafe { (*task.future.get()).layout };
            if let Some(idx) = Scheduler::pool_index(layout) {
                context::LOCAL_TASK_POOL.with(|p| {
                    let mut pool = p[idx].borrow_mut();
                    if pool.len() < 128 {
                        pool.push(task);
                    } else {
                        // Fallback to global pool if local is full.
                        // If global is ALSO full, the task is dropped and memory is freed.
                        let _ = self.handle.scheduler.task_pools[idx].push(task);
                    }
                });
            }
        }
    }

    /// Fast Xorshift32 Random Number Generator
    fn fast_rng(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }

    fn steal(&mut self) -> Option<Arc<Task>> {
        // Find the worker-local queue from thread-local storage safely (for batched stealing)
        let local_queue_ptr = context::LOCAL_QUEUE_PTR.with(|q| q.get());
        if local_queue_ptr.is_null() {
            return None;
        }
        let local_q = unsafe { &mut *local_queue_ptr };

        let start = self.fast_rng() as usize;

        // 1. First, try stealing from other workers (Neighbors) to balance load
        let stealers = &self.handle.scheduler.stealers;
        let len = stealers.len();

        if len > 1 {
            let start_idx = start % len;
            for i in 0..len {
                let idx = (start_idx + i) % len;
                if idx == self.id {
                    continue;
                }

                // Try to steal in batches from other workers
                match stealers[idx].steal_batch_and_pop(local_q) {
                    Steal::Success(task) => return Some(task),
                    Steal::Retry => continue, // Try next worker
                    Steal::Empty => continue,
                }
            }
        }

        // 2. Finally, try stealing from the global injector (Fallback)
        match self.handle.scheduler.queue.steal_batch_and_pop(local_q) {
            Steal::Success(task) => Some(task),
            Steal::Retry => self.steal(), // Recursively try again
            Steal::Empty => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::ready;
    use crate::executor::reactor::Reactor;

    #[test]
    fn test_worker_new() {
        let worker = deque::Worker::new_fifo();
        let reactor = Arc::new(Reactor::new());
        let reactor_notifier = Box::new(|| {});
        let scheduler = Arc::new(Scheduler::new(vec![], vec![], reactor_notifier));
        let handle = Handle::new(scheduler, reactor);
        let id = 0;
        let parker = Parker::new();
        
        let mut worker_instance = Worker::new(id, worker, handle, parker);
        assert_eq!(worker_instance.id, 0);
        assert_eq!(worker_instance.tick, 0);
        assert_eq!(worker_instance.budget, 128);
    }

    #[test]
    fn test_fast_rng() {
        let worker = deque::Worker::new_fifo();
        let reactor = Arc::new(Reactor::new());
        let reactor_notifier = Box::new(|| {});
        let scheduler = Arc::new(Scheduler::new(vec![], vec![], reactor_notifier));
        let handle = Handle::new(scheduler, reactor);
        let mut worker_instance = Worker::new(0, worker, handle, Parker::new());
        
        let rng1 = worker_instance.fast_rng();
        let rng2 = worker_instance.fast_rng();
        let rng3 = worker_instance.fast_rng();
        
        assert_ne!(rng1, rng2);
        assert_ne!(rng2, rng3);
    }

    #[test]
    fn test_execute_task() {
        let worker = deque::Worker::new_fifo();
        let reactor = Arc::new(Reactor::new());
        let reactor_notifier = Box::new(|| {});
        let scheduler = Arc::new(Scheduler::new(vec![], vec![], reactor_notifier));
        let handle = Handle::new(scheduler.clone(), reactor);
        let mut worker_instance = Worker::new(0, worker, handle, Parker::new());
        
        let task = Task::new(ready(()), scheduler, None);
        assert_eq!(task.exec_state.state.load(Ordering::Acquire), STATE_SCHEDULED);
        
        worker_instance.execute(task.clone());
        
        assert_eq!(worker_instance.budget, 127);
        assert_eq!(worker_instance.tick, 1);
    }
}
