use std::{
    cell::UnsafeCell,
    future::Future,
    ptr,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    task::Context,
};

use futures::task::ArcWake;

use crate::executor::scheduler::Scheduler;

pub(crate) const STATE_IDLE: u8 = 0;
pub(crate) const STATE_SCHEDULED: u8 = 1;
pub(crate) const STATE_POLLING: u8 = 2;

pub(crate) const JOIN_STATE_RUNNING: u8 = 0;
pub(crate) const JOIN_STATE_READY: u8 = 1;
pub(crate) const JOIN_STATE_JOINED: u8 = 2;

pub struct Task {
    pub(crate) future: UnsafeCell<RawFuture>,
    scheduler: Arc<Scheduler>,
    pub(crate) state: AtomicU8,
    pub(crate) join_state: AtomicU8,
    pub(crate) result: UnsafeCell<Option<Result<Box<dyn std::any::Any + Send>, crate::executor::join_handle::JoinError>>>,
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
            state: AtomicU8::new(STATE_SCHEDULED),
            join_state: AtomicU8::new(JOIN_STATE_RUNNING),
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
        }
        arc_self.state.store(STATE_SCHEDULED, Ordering::Release);
    }
}

unsafe impl Sync for Task {}
unsafe impl Send for Task {}

impl ArcWake for Task {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        if arc_self.state.swap(STATE_SCHEDULED, Ordering::AcqRel) == STATE_IDLE {
            // Only use the LIFO slot if we are on a worker thread that will check it.
            // This prevents "lost wakeups" where a task is stuck in a non-worker thread's LIFO slot.
            let mut pushed = false;
            
            if crate::executor::context::IS_WORKER.with(|w| w.get()) {
                // 1. Try LIFO slot (highest priority)
                pushed = crate::executor::context::LIFO_SLOT.with(|slot| {
                    let mut slot = slot.borrow_mut();
                    if slot.is_none() {
                        *slot = Some(arc_self.clone());
                        true
                    } else {
                        false
                    }
                });

                // 2. If LIFO is full, try Local Queue (ZERO-OVERHEAD path)
                if !pushed {
                    let local_q_ptr = crate::executor::context::LOCAL_QUEUE_PTR.with(|q| q.get());
                    if !local_q_ptr.is_null() {
                        unsafe {
                            (&mut *local_q_ptr).push(arc_self.clone());
                        }
                        pushed = true;
                    }
                }
            }

            if !pushed {
                arc_self.scheduler.inject(arc_self.clone());
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

    unsafe fn poll<F: Future<Output = ()>>(
        ptr: *mut u8,
        cx: *mut Context<'_>,
    ) -> std::task::Poll<()> {
        let future = unsafe { &mut *(ptr as *mut F) };
        unsafe { std::pin::Pin::new_unchecked(future).poll(&mut *cx) }
    }

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
