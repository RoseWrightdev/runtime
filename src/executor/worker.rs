use std::{
    sync::{atomic::Ordering, Arc},
    task::Context,
};

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
    parker: Parker,
    rng: u32,
}

impl Worker {
    pub fn new(id: usize, queue: deque::Worker<Arc<Task>>, handle: Handle, parker: Parker) -> Self {
        Self {
            id,
            queue: Some(queue),
            handle,
            tick: 0,
            parker,
            rng: (id as u32).wrapping_add(1),
        }
    }

    pub fn run(&mut self) {
        let _guard = crate::executor::context::enter(self.handle.clone());
        // Register this thread as a worker to safely use LIFO slot
        crate::executor::context::enter_worker();

        // Use RAW POINTER for zero-overhead local queue access
        let mut queue = self.queue.take().expect("Worker started without a queue");
        crate::executor::context::LOCAL_QUEUE_PTR.with(|q| q.set(&mut queue as *mut _));

        loop {
            if self.handle.scheduler.shutdown.load(Ordering::Acquire) {
                break;
            }

            // 0. Check LIFO slot for highest priority task (woken by the same thread)
            let task = crate::executor::context::LIFO_SLOT.with(|slot| slot.borrow_mut().take());
            if let Some(task) = task {
                self.execute(task);
                continue;
            }

            // 1. Pop from local queue (ZERO-OVERHEAD path)
            if let Some(task) = queue.pop() {
                self.execute(task);
                continue;
            }

            // 2. Periodically steal from global queue to ensure fairness
            if self.tick % 61 == 0 {
                if let Steal::Success(task) = self.handle.scheduler.steal() {
                    self.execute(task);
                    continue;
                }
            }

            // 3. Steal from other workers or global queue if local is empty
            self.handle.scheduler.searching_workers.fetch_add(1, Ordering::SeqCst);
            if let Some(task) = self.steal() {
                self.handle.scheduler.searching_workers.fetch_sub(1, Ordering::SeqCst);
                self.execute(task);
                continue;
            }

            // If still no work, try a short spin-loop before parking.
            let mut found = false;
            for _ in 0..150 {
                if let Some(task) = self.steal() {
                    self.handle.scheduler.searching_workers.fetch_sub(1, Ordering::SeqCst);
                    self.execute(task);
                    found = true;
                    break;
                }
                if let Steal::Success(task) = self.handle.scheduler.steal() {
                    self.handle.scheduler.searching_workers.fetch_sub(1, Ordering::SeqCst);
                    self.execute(task);
                    found = true;
                    break;
                }
                std::hint::spin_loop();
            }

            if found {
                continue;
            }

            // 4. Fallback to parking or reactor polling
            self.handle.scheduler.searching_workers.fetch_sub(1, Ordering::SeqCst);
            self.handle.scheduler.sleeping_workers.fetch_add(1, Ordering::Release);
            
            // Try to drive the reactor. If we acquire the lock, we wait for I/O events.
            // If another worker is already driving it, we simply park.
            if !self.handle.reactor.try_poll() {
                self.parker.park();
            }

            self.handle.scheduler.sleeping_workers.fetch_sub(1, Ordering::Release);
        }

        // Phase 4: Cleanup raw pointer on exit and restore the queue
        crate::executor::context::LOCAL_QUEUE_PTR.with(|q| q.set(std::ptr::null_mut()));
        self.queue = Some(queue);
    }

    fn execute(&mut self, task: Arc<Task>) {
        if task
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

            // Safety: We have exclusive access to the task because we just
            // transitioned the state from SCHEDULED to POLLING.
            let future = unsafe { &mut *task.future.get() };

            // Register this as the current task to allow self-referencing (for JoinHandle result writing)
            crate::executor::context::CURRENT_TASK.with(|c| *c.borrow_mut() = Some(task.clone()));

            let poll_result = unsafe { (future.poll_fn)(future.ptr, &mut cx) };

            crate::executor::context::CURRENT_TASK.with(|c| *c.borrow_mut() = None);

            if let std::task::Poll::Ready(_) = poll_result {
                unsafe {
                    (future.drop_fn)(future.ptr);
                    future.drop_fn = |_| {};
                    future.poll_fn = |_, _| std::task::Poll::Ready(());
                }

                // Phase 4: Recycle task via Thread-Local Pool first (ZERO contention)
                if Arc::strong_count(&task) == 1 {
                    let layout = unsafe { (*task.future.get()).layout };
                    if let Some(idx) = Scheduler::pool_index(layout) {
                        crate::executor::context::LOCAL_TASK_POOL.with(|p| {
                            let mut pool = p[idx].borrow_mut();
                            if pool.len() < 128 {
                                pool.push(task);
                            } else {
                                // Fallback to global pool if local is full
                                self.handle.scheduler.task_pools[idx].push(task);
                            }
                        });
                    }
                }
            } else {
                // Return to IDLE, unless a wake occurred (state is now SCHEDULED)
                if task
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

    /// Fast Xorshift32 Random Number Generator
    fn fast_rng(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }

    fn steal(&mut self) -> Option<Arc<Task>> {
        // Find the worker-local queue from thread-local storage safely (for batched stealing)
        let local_queue_ptr = crate::executor::context::LOCAL_QUEUE_PTR.with(|q| q.get());
        if local_queue_ptr.is_null() { return None; }
        let local_q = unsafe { &mut *local_queue_ptr };

        // First, try stealing from the global injector in batches (to reduce contention)
        match self.handle.scheduler.queue.steal_batch_and_pop(local_q) {
            Steal::Success(task) => return Some(task),
            Steal::Retry => return self.steal(), // Recursively try again
            Steal::Empty => {}
        }

        let start = self.fast_rng() as usize;

        // Then, try stealing from other workers
        let stealers = &self.handle.scheduler.stealers;

        let len = stealers.len();
        if len <= 1 {
            return None;
        }

        let start_idx = start % len;
        for i in 0..len {
            let idx = (start_idx + i) % len;
            if idx == self.id {
                continue;
            }

            // Also try to steal in batches from other workers
            match stealers[idx].steal_batch_and_pop(local_q) {
                Steal::Success(task) => return Some(task),
                Steal::Retry => continue, // Try next worker
                Steal::Empty => continue,
            }
        }

        None
    }
}
