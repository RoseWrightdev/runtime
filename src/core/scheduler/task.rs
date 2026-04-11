use std::{
    alloc::Layout,
    cell::UnsafeCell,
    future::Future,
    ptr,
    sync::Arc,
    task::Context,
};

use futures::task::ArcWake;
use crate::core::scheduler::scheduler::Scheduler;
use crate::core::executor::context::Context;

pub(crate) struct RawTaskVTable {
    poll_fn: unsafe fn(*mut u8, *mut Context<'_>) -> std::task::Poll<()>,
    drop_fn: unsafe fn(*mut u8),
    dealloc_fn: unsafe fn(*mut u8),
}

pub(crate) trait HasVTable {
    const VTABLE: &'static RawTaskVTable;
}

impl<F: Future<Output = ()> + Send + 'static> HasVTable for F {
    const VTABLE: &'static RawTaskVTable = &RawTaskVTable {
        poll_fn: RawFuture::poll::<F>,
        drop_fn: RawFuture::drop_future::<F>,
        dealloc_fn: RawFuture::dealloc_future::<F>,
    };
}

pub struct RawFuture {
    pub(crate) ptr: *mut u8,
    pub(crate) layout: std::alloc::Layout,
    pub(crate) vtable: &'static RawTaskVTable,
}

impl RawFuture {
    pub fn new<F>(future: F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let layout = std::alloc::Layout::new::<F>();
        let ptr = Context::with(|ctx| ctx.task_pool.allocate(layout));

        unsafe { ptr::write(ptr as *mut F, future); }
        Self { ptr, layout, vtable: <F as HasVTable>::VTABLE }
    }

    #[inline(always)]
    unsafe fn poll<F: Future<Output = ()>>(ptr: *mut u8, cx: *mut Context<'_>) -> std::task::Poll<()> {
        let future = unsafe { &mut *(ptr as *mut F) };
        unsafe { std::pin::Pin::new_unchecked(future).poll(&mut *cx) }
    }

    #[inline(always)]
    unsafe fn drop_future<F>(ptr: *mut u8) {
        unsafe { ptr::drop_in_place(ptr as *mut F) };
    }

    #[inline(always)]
    unsafe fn dealloc_future<F>(ptr: *mut u8) {
        let layout = std::alloc::Layout::new::<F>();
        if layout.size() > 0 {
            Context::with(|ctx| ctx.task_pool.deallocate(ptr, layout));
        }
    }

    pub fn poll_dynamic(&mut self, cx: &mut Context<'_>) -> std::task::Poll<()> {
        unsafe { (self.vtable.poll_fn)(self.ptr, cx as *mut _) }
    }
}

impl Drop for RawFuture {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                (self.vtable.drop_fn)(self.ptr);
                if self.layout.size() > 0 {
                    (self.vtable.dealloc_fn)(self.ptr);
                }
            }
            self.ptr = ptr::null_mut()
        }
    }
}

pub struct Task {
    pub(crate) future: UnsafeCell<RawFuture>,
    scheduler: Arc<Scheduler>,
}

impl Task {
    pub(crate) fn new<F>(future: F, scheduler: Arc<Scheduler>) -> Arc<Self>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Arc::new(Task {
            future: UnsafeCell::new(RawFuture::new(future)),
            scheduler,
        })
    }
}

unsafe impl Sync for Task {}
unsafe impl Send for Task {}

impl ArcWake for Task {
    fn wake_by_ref(_arc_self: &Arc<Self>) {
        // Just push straight to the global scheduler queue 
        // without all the LIFO optimizations for now.
        unimplemented!()
    }
}
