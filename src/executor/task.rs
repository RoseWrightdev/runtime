use std::{
    cell::UnsafeCell,
    future::Future,
    ptr,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    task::Context,
    any::Any
};

use crossbeam::utils::CachePadded;
use futures::task::ArcWake;

use crate::executor::scheduler::Scheduler;
use crate::executor::join_handle::JoinError;
use crate::executor::context;

/// Task is idle and not currently in any queue.
pub(crate) const STATE_IDLE: u8 = 0;
/// Task is scheduled for execution and is in a queue (local or global).
pub(crate) const STATE_SCHEDULED: u8 = 1;
/// Task is currently being polled by a worker.
pub(crate) const STATE_POLLING: u8 = 2;

/// Task is currently running and has not yet produced a result.
pub(crate) const JOIN_STATE_RUNNING: u8 = 0;
/// Task has completed and the result is available in the `result` field.
pub(crate) const JOIN_STATE_READY: u8 = 1;
/// The result has been consumed by a `JoinHandle`.
pub(crate) const JOIN_STATE_JOINED: u8 = 2;


/// Tracks the execution status and priority of a task.
pub(crate) struct ExecutionState {
    /// The current execution state (IDLE, SCHEDULED, or POLLING).
    pub(crate) state: AtomicU8,
    /// Number of consecutive times this task has been woken into the LIFO slot.
    /// Used to prevent starvation of other tasks.
    pub(crate) lifo_count: AtomicU8,
}

/// The unit of execution in the Taiga runtime.
/// 
/// A `Task` encapsulates a future, its execution state, and storage for its 
/// eventual output. It provides the necessary metadata for the scheduler 
/// to manage asynchronous execution across multiple threads.
/// 
/// ## Type Erasure and Memory Management
/// 
/// Since Rust futures are anonymous and uniquely typed, Taiga uses 
/// **Type Erasure** (via [`RawFuture`]). This allows diverse future types 
/// to be stored uniformly in task queues. Additionally, `Task` supports 
/// **In-place Re-initialization** (Recycling), which bypasses heap allocation 
/// by reusing the memory of completed tasks for new futures of compatible 
/// layout.
pub struct Task {
    /// The underlying future, type-erased via [`RawFuture`].
    pub(crate) future: UnsafeCell<RawFuture>,
    /// The scheduler responsible for this task.
    pub(crate) scheduler: Arc<Scheduler>,
    /// Execution-related state (IDLE, SCHEDULED, or POLLING).
    pub(crate) exec_state: CachePadded<ExecutionState>,
    /// Join-related state (RUNNING, READY, or JOINED).
    pub(crate) join_state: CachePadded<AtomicU8>,
    /// Storage for the future's output or a panic error.
    /// 
    /// This uses `dyn Any` so it can store literally any return type.
    pub(crate) result: UnsafeCell<Option<Result<Box<dyn Any + Send>, JoinError>>>,
    /// The waker for a `JoinHandle` awaiting this task.
    pub(crate) join_waker: CachePadded<futures::task::AtomicWaker>,
}

impl Task {
    /// Creates a new task from a future.
    pub(crate) fn new<F>(
        future: F,
        scheduler: Arc<Scheduler>,
        layout: Option<std::alloc::Layout>,
    ) -> Arc<Self>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Arc::new(Task {
            future: UnsafeCell::new(RawFuture::new(future, layout)),
            scheduler,
            exec_state: CachePadded::new(ExecutionState {
                state: AtomicU8::new(STATE_SCHEDULED),
                lifo_count: AtomicU8::new(0),
            }),
            join_state: CachePadded::new(AtomicU8::new(JOIN_STATE_RUNNING)),
            result: UnsafeCell::new(None),
            join_waker: CachePadded::new(futures::task::AtomicWaker::new()),
        })
    }

    /// Re-initializes an existing task with a new future.
    /// 
    /// This is an optimization! Instead of throwing away the "Task" object and 
    /// creating a new one (which involves memory allocation), we just wipe the 
    /// old one clean and put a new future inside it.
    pub(crate) fn reuse<F>(arc_self: &Arc<Self>, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // SAFETY: We have unique access to the task's internal state because 
        // the task has completed (or is being initialized) and we are the 
        // sole owners of the Arc or have verified the task state is IDLE.
        unsafe {
            let raw = &mut *arc_self.future.get();
            raw.recondition(future);
            arc_self.join_waker.take();
            *arc_self.result.get() = None;
        }
        arc_self.exec_state.lifo_count.store(0, Ordering::Relaxed);
        arc_self.join_state.store(JOIN_STATE_RUNNING, Ordering::Release);
        arc_self.exec_state.state.store(STATE_SCHEDULED, Ordering::Release);
    }
}

// SAFETY: `Task` handles its own internal synchronization via atomics 
// and `UnsafeCell` access is guarded by the `exec_state` (IDLE, SCHEDULED, POLLING).
unsafe impl Sync for Task {}
// SAFETY: `Task` is designed to be moved between threads by the scheduler.
unsafe impl Send for Task {}

impl ArcWake for Task {
    fn wake(self: Arc<Self>) {
        Self::wake_by_ref(&self)
    }

    fn wake_by_ref(arc_self: &Arc<Self>) {
        if arc_self.exec_state.state.swap(STATE_SCHEDULED, Ordering::AcqRel) == STATE_IDLE {
            // Only use the LIFO slot if we are on a worker thread that will check it.
            // This prevents "lost wakeups" where a task is stuck in a non-worker thread's LIFO slot.
            let mut pushed = false;

            if context::IS_WORKER.get() {
                // 1. Try LIFO slot (highest priority)
                // If the task has been woken more than 3 times consecutively into the LIFO slot,
                // bypass the slot and push it to the local deque to ensure other tasks aren't starved.
                if arc_self.exec_state.lifo_count.load(Ordering::Relaxed) < 3 {
                    pushed = context::LIFO_SLOT.with(|slot| {
                        let mut slot = slot.borrow_mut();
                        if slot.is_none() {
                            *slot = Some(arc_self.clone());
                            true
                        } else {
                            false
                        }
                    });
                }

                if pushed {
                    arc_self.exec_state.lifo_count.fetch_add(1, Ordering::Relaxed);
                } else {
                    // 2. If LIFO is full or bypassed, try Local Queue (ZERO-OVERHEAD path)
                    let local_q_ptr = context::LOCAL_QUEUE_PTR.get();
                    if !local_q_ptr.is_null() {
                        // SAFETY: `LOCAL_QUEUE_PTR` is only set on worker threads 
                        // and points to a valid `deque::Worker` owned by that thread.
                        unsafe {
                            (&mut *local_q_ptr).push(arc_self.clone());
                        }
                        pushed = true;
                        arc_self.exec_state.lifo_count.store(0, Ordering::Relaxed);
                    }
                }
            }

            if !pushed {
                arc_self.scheduler.inject(arc_self.clone());
                arc_self.exec_state.lifo_count.store(0, Ordering::Relaxed);
            }
        }
    }
}

/// A type-erased future implementation.
/// 
/// `RawFuture` enables heterogeneous futures to be managed via a stable ABI. 
/// It stores a pointer to the future's state on the heap and utilizes 
/// dynamic dispatch via function pointers for polling and destruction.
/// 
/// This manual vtable approach provides maximum performance and allows for 
/// the task recycling optimizations used throughout the runtime.
pub struct RawFuture {
    /// Pointer to the future's state on the heap.
    pub(crate) ptr: *mut u8,
    /// The memory layout (size) of the future.
    pub(crate) layout: std::alloc::Layout,
    /// Function pointer to "press the Run button" (Poll).
    pub(crate) poll_fn: unsafe fn(*mut u8, *mut Context<'_>) -> std::task::Poll<()>,
    /// Function pointer to "press the Delete button" (Drop).
    pub(crate) drop_fn: unsafe fn(*mut u8),
}


impl RawFuture {
    /// Allocates and initializes a new `RawFuture`.
    pub fn new<F>(future: F, layout: Option<std::alloc::Layout>) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let layout = layout.unwrap_or_else(|| std::alloc::Layout::new::<F>());
        let ptr = if layout.size() == 0 {
            layout.align() as *mut u8
        } else {
            // SAFETY: `layout` is a valid `std::alloc::Layout` for type `F`.
            let p = unsafe { std::alloc::alloc(layout) };
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            p
        };

        // SAFETY: `ptr` points to a newly allocated (or ZST) block of memory 
        // with sufficient size and alignment for `F`.
        unsafe {
            ptr::write(ptr as *mut F, future);
        }

        RawFuture {
            ptr,
            layout,
            poll_fn: Self::poll::<F>,
            drop_fn: Self::drop_future::<F>,
        }
    }

    /// Safely re-initialize an existing RawFuture with a new Future state.
    /// This bypasses the standard allocator and overwrites the existing memory block.
    /// 
    /// # Safety
    /// 
    /// The caller must ensure:
    /// 1. `self.layout` is large enough and has sufficient alignment for `F`.
    /// 2. The previous future in `self.ptr` has been dropped.
    pub unsafe fn recondition<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // SAFETY: The runtime verifies layout compatibility by checking 
        // the pool_index bucket before reuse. The caller ensures the 
        // previous future was dropped (e.g., in `Worker::execute`).
        unsafe {
            ptr::write(self.ptr as *mut F, future);
        }
        self.poll_fn = Self::poll::<F>;
        self.drop_fn = Self::drop_future::<F>;
    }

    /// # Safety
    /// 
    /// `ptr` must point to a valid instance of `F`. `cx` must be a valid 
    /// pointer to a `Context`.
    #[inline(always)]
    unsafe fn poll<F: Future<Output = ()>>(
        ptr: *mut u8,
        cx: *mut Context<'_>,
    ) -> std::task::Poll<()> {
        // SAFETY: The caller ensures `ptr` is a valid pointer to `F` 
        // and that it is not currently being accessed elsewhere.
        let future = unsafe { &mut *(ptr as *mut F) };
        // SAFETY: Pinning is safe here as the future's memory location 
        // in `RawFuture` is stable until dropped.
        unsafe { std::pin::Pin::new_unchecked(future).poll(&mut *cx) }
    }

    /// # Safety
    /// 
    /// `ptr` must point to a valid instance of `F`.
    #[inline(always)]
    unsafe fn drop_future<F>(ptr: *mut u8) {
        // SAFETY: The caller ensures `ptr` points to a valid `F` 
        // and that it will not be used after this call.
        unsafe { ptr::drop_in_place(ptr as *mut F) };
    }
}

impl Drop for RawFuture {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: `self.ptr` is a valid pointer to the future's state 
            // allocated in `RawFuture::new`. `drop_fn` is the correct 
            // destructor for the underlying type.
            unsafe {
                (self.drop_fn)(self.ptr);
                if self.layout.size() > 0 {
                    std::alloc::dealloc(self.ptr, self.layout);
                }
            }
            // reset pointer to null
            self.ptr = ptr::null_mut()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_new() {
        let reactor_notifier = Box::new(|| {});
        let scheduler = Arc::new(Scheduler::new(vec![], vec![], reactor_notifier));
        let task = Task::new(async {}, scheduler, None);
        assert_eq!(task.exec_state.state.load(Ordering::Acquire), STATE_SCHEDULED);
        assert_eq!(task.join_state.load(Ordering::Acquire), JOIN_STATE_RUNNING);
    }

    #[test]
    fn test_raw_future_lifecycle() {
        let raw = RawFuture::new(async { }, None);
        assert!(!raw.ptr.is_null());
        // Dropping should happen automatically
    }

    #[test]
    fn test_task_reuse() {
        let reactor_notifier = Box::new(|| {});
        let scheduler = Arc::new(Scheduler::new(vec![], vec![], reactor_notifier));
        let task = Task::new(async {}, scheduler, None);
        
        // Mark as idle to simulate completion
        task.exec_state.state.store(STATE_IDLE, Ordering::Release);
        
        Task::reuse(&task, async { });
        assert_eq!(task.exec_state.state.load(Ordering::Acquire), STATE_SCHEDULED);
    }
}
