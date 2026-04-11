pub mod core;
pub mod net;
pub mod time;

pub use crate::core::runtime::runtime::Runtime;
use crate::core::runtime::context::Context as RuntimeContext;

use std::future::Future;

pub fn spawn<F, T>(future: F)
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if let Some(scheduler) = RuntimeContext::current() {
        scheduler.spawn_internal(future);
    } else {
        panic!("spawn called outside of taiga runtime context");
    }
}

pub fn block_on<F>(future: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Runtime::new().block_on(future)
}

#[cfg(test)]
mod tests {}