use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::executor::Handle;

/// A future that resolves after a specified duration.
/// 
/// `Sleep` implements an asynchronous timer by registering a deadline with 
/// the [`Reactor`][crate::executor::Reactor]. When the deadline is reached, the Reactor triggers the 
/// waker associated with the task, allowing the executor to re-schedule it.
pub struct Sleep {
    /// The instant at which the timer should fire.
    deadline: Instant,
    /// Flag indicating whether the timer has been registered with the reactor.
    registered: bool,
}

impl Sleep {
    pub fn new(duration: Duration) -> Self {
        Self {
            deadline: Instant::now() + duration,
            registered: false,
        }
    }
}

impl Future for Sleep {
    type Output = ();
    
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // 1. Check if the deadline has already been reached.
        if Instant::now() >= self.deadline {
            return Poll::Ready(());
        }

        // 2. Register the timer with the Reactor if not already registered.
        if !self.registered {
            let handle = Handle::current();
            handle.register_timer(self.deadline, cx.waker().clone());
            self.registered = true;
        }

        // 3. Yield execution until the timer expires.
        Poll::Pending
    }
}

/// Asynchronously waits for a specified duration.
pub fn sleep(duration: Duration) -> Sleep {
    Sleep::new(duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Runtime;
    use std::time::Duration;

    #[test]
    fn test_sleep_basic() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let start = Instant::now();
            sleep(Duration::from_millis(50)).await;
            let elapsed = start.elapsed();
            assert!(elapsed >= Duration::from_millis(50));
        });
    }

    #[test]
    fn test_sleep_zero() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            sleep(Duration::from_millis(0)).await;
        });
    }
}
