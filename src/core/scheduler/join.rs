use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::ptr;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll, Waker};

use crate::core::scheduler::task::TaskRef;

pub struct JoinError {
    _private: (),
}

impl std::fmt::Debug for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinError").finish()
    }
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JoinError: task panicked or was cancelled")
    }
}

impl std::error::Error for JoinError {}

pub struct JoinHandle<T> {
    task: TaskRef,
    _marker: PhantomData<T>,
}

unsafe impl<T: Send> Send for JoinHandle<T> {}
unsafe impl<T: Sync> Sync for JoinHandle<T> {}

impl<T> JoinHandle<T> {
    pub(crate) fn new(task: TaskRef) -> Self {
        Self {
            task,
            _marker: PhantomData,
        }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let header = unsafe { self.task.as_ptr().as_ref() };

        // Check if finished
        let state = header.result_state.load(Ordering::Acquire);
        if state == 1 {
            // Completed
            // Move result out
            unsafe {
                let mut out = std::mem::MaybeUninit::<T>::uninit();
                (header.vtable.read_result)(self.task.as_ptr(), out.as_mut_ptr() as *mut u8);
                return Poll::Ready(Ok(out.assume_init()));
            }
        } else if state == 2 {
            // Panicked
            return Poll::Ready(Err(JoinError { _private: () }));
        } else if state == 3 {
            // Already joined
            panic!("JoinHandle polled after completion");
        }

        // Not finished, register waker
        let waker_ptr = Box::into_raw(Box::new(cx.waker().clone())) as *mut ();
        let old_waker_ptr = header.join_waker.swap(waker_ptr, Ordering::SeqCst);

        if !old_waker_ptr.is_null() {
            unsafe {
                let _ = Box::from_raw(old_waker_ptr as *mut Waker);
            }
        }

        // Double check state to avoid race
        let state = header.result_state.load(Ordering::Acquire);
        if state != 0 {
            // Already finished, wake ourselves to re-poll and take the result
            let waker_ptr = header.join_waker.swap(ptr::null_mut(), Ordering::SeqCst);
            if !waker_ptr.is_null() {
                unsafe {
                    let waker = Box::from_raw(waker_ptr as *mut Waker);
                    waker.wake();
                }
            }
        }

        Poll::Pending
    }
}
