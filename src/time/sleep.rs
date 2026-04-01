use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::executor::Handle;

/// A future that resolves after a specified duration.
pub struct Sleep {
    deadline: Instant,
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
        if Instant::now() >= self.deadline {
            return Poll::Ready(());
        }

        if !self.registered {
            // Register this timer with the current runtime's reactor via the buffered handle
            let handle = Handle::current();
            handle.register_timer(self.deadline, cx.waker().clone());
            self.registered = true;
        }

        Poll::Pending
    }
}

/// Asynchronously waits for a specified duration.
pub fn sleep(duration: Duration) -> Sleep {
    Sleep::new(duration)
}
