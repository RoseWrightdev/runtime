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

pub(crate) const STATE_IDLE: u8 = 0;
pub(crate) const STATE_SCHEDULED: u8 = 1;
pub(crate) const STATE_POLLING: u8 = 2;

pub(crate) const JOIN_STATE_RUNNING: u8 = 0;
pub(crate) const JOIN_STATE_READY: u8 = 1;
pub(crate) const JOIN_STATE_JOINED: u8 = 2;


pub(crate) struct ExecutionState {
    pub(crate) state: AtomicU8,
    pub(crate) lifo_count: AtomicU8,
}

pub struct Task {
    pub(crate) future: UnsafeCell<RawFuture>,
    pub(crate) scheduler: Arc<Scheduler>,
    pub(crate) exec_state: CachePadded<ExecutionState>,
    pub(crate) join_state: CachePadded<AtomicU8>,
    pub(crate) result: UnsafeCell<Option<Result<Box<dyn Any + Send>, JoinError>>>,
    pub(crate) join_waker: UnsafeCell<Option<std::task::Waker>>,
}

impl Task {
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
            join_waker: UnsafeCell::new(None),
        })
    }

    pub(crate) fn reuse<F>(arc_self: &Arc<Self>, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        unsafe {
            let raw = &mut *arc_self.future.get();
            raw.recondition(future);
            *arc_self.join_waker.get() = None;
            *arc_self.result.get() = None;
        }
        arc_self.exec_state.lifo_count.store(0, Ordering::Relaxed);
        arc_self.join_state.store(JOIN_STATE_RUNNING, Ordering::Release);
        arc_self.exec_state.state.store(STATE_SCHEDULED, Ordering::Release);
    }
}

unsafe impl Sync for Task {}
unsafe impl Send for Task {}

impl ArcWake for Task {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        if arc_self.exec_state.state.swap(STATE_SCHEDULED, Ordering::AcqRel) == STATE_IDLE {
            // Only use the LIFO slot if we are on a worker thread that will check it.
            // This prevents "lost wakeups" where a task is stuck in a non-worker thread's LIFO slot.
            let mut pushed = false;

            if crate::executor::context::IS_WORKER.with(|w| w.get()) {
                // 1. Try LIFO slot (highest priority)
                // If the task has been woken more than 3 times consecutively into the LIFO slot,
                // bypass the slot and push it to the local deque to ensure other tasks aren't starved.
                if arc_self.exec_state.lifo_count.load(Ordering::Relaxed) < 3 {
                    pushed = crate::executor::context::LIFO_SLOT.with(|slot| {
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
                    let local_q_ptr = crate::executor::context::LOCAL_QUEUE_PTR.with(|q| q.get());
                    if !local_q_ptr.is_null() {
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

    fn wake(self: Arc<Self>) {
        Self::wake_by_ref(&self)
    }
}

pub struct RawFuture {
    pub(crate) ptr: *mut u8,
    pub(crate) layout: std::alloc::Layout,
    pub(crate) poll_fn: unsafe fn(*mut u8, *mut Context<'_>) -> std::task::Poll<()>,
    pub(crate) drop_fn: unsafe fn(*mut u8),
}

impl RawFuture {
    pub fn new<F>(future: F, layout: Option<std::alloc::Layout>) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let layout = layout.unwrap_or_else(|| std::alloc::Layout::new::<F>());
        let ptr = if layout.size() == 0 {
            layout.align() as *mut u8
        } else {
            let p = unsafe { std::alloc::alloc(layout) };
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            p
        };

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
    pub unsafe fn recondition<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Safety: We must ensure self.layout is large enough for std::alloc::Layout::new::<F>().
        // The runtime verifies this by checking the pool_index bucket before reuse.
        unsafe {
            ptr::write(self.ptr as *mut F, future);
        }
        self.poll_fn = Self::poll::<F>;
        self.drop_fn = Self::drop_future::<F>;
    }

    #[inline(always)]
    unsafe fn poll<F: Future<Output = ()>>(
        ptr: *mut u8,
        cx: *mut Context<'_>,
    ) -> std::task::Poll<()> {
        let future = unsafe { &mut *(ptr as *mut F) };
        unsafe { std::pin::Pin::new_unchecked(future).poll(&mut *cx) }
    }

    #[inline(always)]
    unsafe fn drop_future<F>(ptr: *mut u8) {
        unsafe { ptr::drop_in_place(ptr as *mut F) };
    }
}

impl Drop for RawFuture {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
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
