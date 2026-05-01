use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{atomic::Ordering, Arc};
use std::task::{Context, Poll};

use crate::executor::task::{Task, JOIN_STATE_READY, JOIN_STATE_JOINED};

pub type JoinError = Box<dyn Any + Send + 'static>;

/// A handle that allows awaiting the result of a spawned task.
///
/// `JoinHandle` is generic over the task's return type `T`. However, the underlying
/// [`Task`] is type-erased to allow it to be managed by the scheduler. `JoinHandle`
/// acts as the bridge between the type-erased execution and the typed result.
pub struct JoinHandle<T> {
    /// The type-erased task.
    pub(crate) task: Arc<Task>,
    /// A marker to "remember" the return type `T`. 
    ///
    /// This is necessary because `T` is not used in any other field (type erasure),
    /// and it ensures the compiler understands the variance of `T` and allows
    /// us to safely downcast the result when the task completes.
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
        // 1. Try to transition to JOINED.
        // We use AcqRel (Acquire-Release) to ensure:
        // - Release: Any results written to `task.result` by the completing worker 
        //   are visible to us before we take ownership.
        // - Acquire: We see the state update from READY to JOINED.
        if self.task.join_state.compare_exchange(
            JOIN_STATE_READY,
            JOIN_STATE_JOINED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ).is_ok() {
            // Success! The result is now owned by us.
            // SAFETY: We have successfully transitioned the task state from READY 
            // to JOINED using an atomic compare-exchange, ensuring we have 
            // exclusive ownership of the result.
            let result = unsafe { &mut *self.task.result.get() }.take().unwrap();
            return Poll::Ready(result.map(|val| {
                *val.downcast::<T>().expect("JoinHandle type mismatch")
            }));
        }

        // 2. If already joined or running, check state.
        // Acquire ordering ensures we see the most up-to-date state of the task 
        // before making a decision.
        let state = self.task.join_state.load(Ordering::Acquire);
        if state == JOIN_STATE_READY {
            // Re-poll to perform the transition safely with ownership Transfer
            return self.poll(cx);
        }
        if state == JOIN_STATE_JOINED {
             panic!("JoinHandle polled after completion");
        }

        // 3. Register our waker
        self.task.join_waker.register(cx.waker());

        // 4. Double-check state in case it finished during registration (to avoid lost wakeups).
        // Again, Acquire ensures we see any result visibility if the state became READY.
        if self.task.join_state.load(Ordering::Acquire) == JOIN_STATE_READY {
            // Re-poll to perform the transition
            return self.poll(cx);
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use crate::executor::Runtime;

    #[test]
    fn test_join_handle_success() {
        let runtime = Runtime::new();
        let handle = runtime.spawn(async { 42 });
        let result = runtime.block_on(handle).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_join_handle_panic() {
        let runtime = Runtime::new();
        let handle = runtime.spawn(async { 
            panic!("Task panicked"); 
        });
        let result = runtime.block_on(handle);
        assert!(result.is_err());
    }
}
