pub mod executor;
pub mod time;
pub mod net;

pub use executor::{Runtime, Task, RawFuture, Handle, JoinHandle};
pub use time::sleep;
pub use net::{AsyncTcpListener, AsyncTcpStream};

use std::future::Future;

/// Spawns a task on the current runtime.
pub fn spawn<F, T>(future: F) -> JoinHandle<T>
where 
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    Handle::current().spawn(future)
}

/// Runs a future to completion on a new runtime.
pub fn block_on<F, T>(future: F) -> T
where 
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let rt = Runtime::new();
    rt.block_on(future)
}

#[cfg(test)]
mod tests;