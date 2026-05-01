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
    Handle, Scheduler, Task, stats,
};

/// The execution loop that runs on each background thread.
/// 
/// The `Worker` implements the primary polling loop for each execution thread. 
/// Its execution strategy follows a strict priority order:
/// 
/// 1.  **LIFO Polling**: Checks the thread-local LIFO slot for immediate re-execution.
/// 2.  **Local Deque Polling**: Checks the thread-local work-stealing deque.
/// 3.  **Global Injection/Stealing**: Attempts to acquire work from the global 
///     injector or by stealing from other workers.
/// 4.  **Wait (Parking)**: If no work is found, the thread parks until 
///     triggered by an external event or task injection.
pub struct Worker {
    /// Unique ID for this worker thread.
    id: usize,
    /// The private task queue for this worker.
    queue: Option<deque::Worker<Arc<Task>>>,
    /// A handle back to the runtime.
    handle: Handle,
    /// Counter used for periodic maintenance tasks (like checking the global queue).
    tick: usize,
    /// A "fuel" counter that prevents a single task from hogging the thread forever.
    budget: u8,
    /// Mechanism to put the thread to sleep and wake it up.
    parker: Parker,
    /// Local random number generator for picking which worker to steal from.
    rng: u32,
    /// Counter for consecutive LIFO polls.
    lifo_count: u8,
    /// Statistics for dynamic scheduler tuning.
    stats: stats::Stats,
}

#[inline(always)]
fn prefetch<T>(ptr: *const T) {
    // SAFETY: The assembly instructions used here are non-destructive 
    // prefetch hints and do not affect the architectural state of the CPU.
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
            lifo_count: 0,
            stats: stats::Stats::new(),
        }
    }

    /// The main loop of the worker thread.
    pub fn run(&mut self) {
        let _guard = context::enter(self.handle.clone());
        // Register this thread as a worker to safely use LIFO slot
        context::enter_worker();

        // Use RAW POINTER for zero-overhead local queue access
        let mut queue = self.queue.take().expect("Worker started without a queue");
        context::LOCAL_QUEUE_PTR.set(&mut queue as *mut _);

        loop {
            // Check if the whole runtime is shutting down.
            // Acquire ensures we see the latest shutdown state.
            if self.handle.scheduler.shutdown.load(Ordering::Acquire) {
                break;
            }

            // 1. Check if we've been running too long without a break
            self.cooperative_yield(&mut queue);

            // 2. Try to find a task to run
            if let Some(task) = self.next_local_task(&mut queue) {
                self.execute(task);
                continue;
            }

            self.stats.end_batch();
            self.search_and_park();
            self.stats.start_batch();
        }

        // Cleanup raw pointer on exit and restore the queue
        context::LOCAL_QUEUE_PTR.set(std::ptr::null_mut());
        self.queue = Some(queue);
    }

    /// Implements "Cooperative Multitasking."
    /// 
    /// In Rust async, a task can theoretically run forever if it never `awaits`. 
    /// This would "starve" other tasks. The `budget` ensures that after 128 
    /// steps, we force the current thread to take a break and let others run.
    fn cooperative_yield(&mut self, queue: &mut deque::Worker<Arc<Task>>) {
        if self.budget == 0 {
            self.budget = 128;
            let task = context::LIFO_SLOT
                .with(|slot| slot.borrow_mut().take())
                .or_else(|| queue.pop());

            if let Some(task) = task {
                // Push the task back to the global queue so others can help out
                self.handle.scheduler.inject(task);
            }
        }
    }

    /// Finds the next task to run, prioritizing local work for speed.
    fn next_local_task(&mut self, queue: &mut deque::Worker<Arc<Task>>) -> Option<Arc<Task>> {
        // 1. Check LIFO slot (highest priority, best cache locality)
        // We cap this at 3 consecutive polls to prevent starvation of other tasks.
        if self.lifo_count < 3 {
            let task = context::LIFO_SLOT.with(|slot| slot.borrow_mut().take());
            if let Some(task) = task {
                self.lifo_count += 1;
                return Some(task);
            }
        }

        // Reset LIFO count if we pull from anywhere else.
        self.lifo_count = 0;

        // 2. Pop from my own private local queue
        if let Some(task) = queue.pop() {
            return Some(task);
        }

        // 3. Periodically check the global queue to make sure those tasks aren't forgotten.
        // We use batch stealing here to reduce atomic contention on the global injector.
        // The interval is dynamically tuned based on task poll times.
        if self.tick % self.stats.tuned_interval() as usize == 0 {
            if let Steal::Success(task) = self.handle.scheduler.queue.steal_batch_and_pop(queue) {
                return Some(task);
            }
        }

        None
    }

    /// The "Work-Stealing" and "Parking" logic.
    fn search_and_park(&mut self) {
        // 1. Try to steal from other workers or the global queue.
        // We use Relaxed for searching_workers as it's a heuristic counter 
        // used for throttling notifications, not for memory safety.
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

        // 2. If still no work, try "spinning" (looping quickly) for a moment. 
        // This is often faster than going to sleep if work arrives soon.
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

        // 3. Still nothing? Prepare to go to sleep.
        self.handle.scheduler.searching_workers.fetch_sub(1, Ordering::Relaxed);

        // One final attempt to steal from the global injector if searching count is low.
        // This mitigates the "last searcher" race where a task is injected just as we park.
        // Acquire ensures we see the most recent worker states.
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

        // We use Release when adding to sleeping_workers to ensure any 
        // local state changes are visible if another thread notifies us.
        self.handle.scheduler.sleeping_workers.fetch_add(1, Ordering::Release);

        // Send any pending networking/timer requests to the Reactor
        self.handle.flush_registrations();

        // One worker is always responsible for "driving" the Reactor (watching I/O).
        // If nobody else is doing it, I'll do it while I wait.
        if !self.handle.reactor.try_poll() {
            self.handle.flush_registrations();
            // Someone else is driving the reactor, so I'll just truly sleep.
            self.parker.park();
        }

        self.handle
            .scheduler
            .sleeping_workers
            .fetch_sub(1, Ordering::Release);
    }

    /// Executes a single task.
    fn execute(&mut self, task: Arc<Task>) {

        // Attempt to transition the task from SCHEDULED to POLLING.
        // - AcqRel: Ensure we see previous writes to the task (Acquire) and 
        //   our state change is visible to others (Release).
        // - Acquire: On failure, ensure we see the updated state.
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
            // SAFETY: We have successfully transitioned the task state to POLLING, 
            // giving us exclusive access to the future's internal state.
            let future = unsafe { &mut *task.future.get() };

            // Decrement budget upon execution
            self.budget = self.budget.saturating_sub(1);

            // Register this as the current task to allow self-referencing (for JoinHandle result writing)
            context::CURRENT_TASK.with(|c| *c.borrow_mut() = Some(task.clone()));

            // SAFETY: `poll_fn` is a valid function pointer to the future's 
            // poll implementation, and `ptr` is a valid pointer to its state.
            let poll_result = unsafe { (future.poll_fn)(future.ptr, &mut cx) };
            self.stats.record_poll();
            context::CURRENT_TASK.with(|c| *c.borrow_mut() = None);

            if let std::task::Poll::Ready(_) = poll_result {
                // SAFETY: The future has completed, so we can safely drop it. 
                // We reset the function pointers to prevent double-dropping.
                unsafe {
                    (future.drop_fn)(future.ptr);
                    future.drop_fn = |_| {};
                    future.poll_fn = |_, _| std::task::Poll::Ready(());
                }

                self.recycle_task(task);
            } else {
                // Return to IDLE, unless a wake occurred (state is now SCHEDULED).
                // Use AcqRel to ensure the state transition is synchronized.
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
            // SAFETY: We are the sole owner of the task (strong_count == 1), 
            // so we can safely access its internal layout.
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
        // SAFETY: `local_queue_ptr` is retrieved from `LOCAL_QUEUE_PTR`, 
        // which is only set on worker threads and points to a valid 
        // `deque::Worker` owned by the current thread.
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
        
        let worker_instance = Worker::new(id, worker, handle, parker);
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
