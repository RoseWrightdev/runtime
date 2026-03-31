use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{atomic::Ordering, Arc};
use std::task::{Context, Poll};

use crate::executor::task::{Task, JOIN_STATE_READY, JOIN_STATE_JOINED};

pub type JoinError = Box<dyn Any + Send + 'static>;

pub struct JoinHandle<T> {
    pub(crate) task: Arc<Task>,
    pub(crate) _marker: PhantomData<T>,
}

impl<T> JoinHandle<T> {
    pub(crate) fn new(task: Arc<Task>) -> Self {
        Self {
            task,
            _marker: PhantomData,
        }
    }
}

impl<T: Any + Send + 'static> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // 1. Try to transition to JOINED
        if self.task.join_state.compare_exchange(
            JOIN_STATE_READY,
            JOIN_STATE_JOINED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ).is_ok() {
            // Success! The result is now owned by us.
            let result = unsafe { &mut *self.task.result.get() }.take().unwrap();
            return Poll::Ready(result.map(|val| {
                *val.downcast::<T>().expect("JoinHandle type mismatch")
            }));
        }

        // 2. If already joined or running, check state
        let state = self.task.join_state.load(Ordering::Acquire);
        if state == JOIN_STATE_READY {
            // Re-poll to perform the transition safely with ownership Transfer
            return self.poll(cx);
        }
        if state == JOIN_STATE_JOINED {
             panic!("JoinHandle polled after completion");
        }

        // 3. Register our waker
        unsafe {
            *self.task.join_waker.get() = Some(cx.waker().clone());
        }

        // 4. Double-check state in case it finished during registration (to avoid lost wakeups)
        if self.task.join_state.load(Ordering::Acquire) == JOIN_STATE_READY {
            // Re-poll to perform the transition
            return self.poll(cx);
        }

        Poll::Pending
    }
}
