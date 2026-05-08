#![warn(clippy::pedantic)]

//! # Taiga Runtime
//! 
//! Taiga is a high-performance, asynchronous runtime for Rust, designed to be both 
//! extremely fast and easy to understand. 
//! 
//! ## What is an Asynchronous Runtime?
//! 
//! In traditional programming, if you want to perform two tasks at once (like 
//! downloading two files), you might use two separate threads. However, threads are 
//! "heavy"—they use a lot of memory and switching between them takes time.
//! 
//! An **asynchronous runtime** allows you to run thousands of tasks on just a few 
//! threads. It does this by "pausing" tasks when they are waiting for something 
//! (like data from a network) and using that thread to run a different task in 
//! the meantime.
//! 
//! ## Core Components
//! 
//! - **Executor**: Orchestrates task lifecycle and thread distribution.
//! - **Reactor**: Interfaces with OS kernel event loops for I/O and timers.
//! - **Tasks**: Encapsulated units of execution (Futures) managed by the runtime.
//! 
//! ## Getting Started
//! 
//! The simplest way to use Taiga is via [`spawn`] and [`block_on`].
//! 
//! ```rust
//! use runtime::{spawn, block_on};
//! 
//! block_on(async {
//!     let handle = spawn(async {
//!         println!("Hello from a task!");
//!         42
//!     });
//! 
//!     let result = handle.await.unwrap();
//!     assert_eq!(result, 42);
//! });
//! ```

pub mod executor;
pub mod time;
pub mod net;

pub use executor::{Runtime, Task, RawFuture, Handle, JoinHandle};
pub use time::sleep;
pub use net::{AsyncTcpListener, AsyncTcpStream};

use std::future::Future;

/// Spawns a new task on the current runtime.
/// 
/// This is the primary way to run code concurrently in Taiga. When you `spawn` 
/// a future, the runtime takes ownership of it and schedules it to run on 
/// one of its worker threads.
/// 
/// Returns a [`JoinHandle`] which allows you to `await` the result of the task 
/// or cancel it.
/// 
/// # Examples
/// 
/// ```rust
/// use runtime::{spawn, block_on};
/// 
/// block_on(async {
///     let handle = spawn(async {
///         // Do some work...
///         "Success!"
///     });
///     assert_eq!(handle.await.unwrap(), "Success!");
/// });
/// ```
pub fn spawn<F, T>(future: F) -> JoinHandle<T>
where 
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    Handle::current().spawn(future)
}

/// Runs a future to completion on a new runtime, blocking the current thread.
/// 
/// This is the entry point for executing asynchronous code from a synchronous 
/// context. It initializes a new [`Runtime`] instance and blocks the current 
/// thread until the provided future resolves.
/// 
/// # Examples
/// 
/// ```rust
/// use runtime::block_on;
/// 
/// fn main() {
///     block_on(async {
///         println!("Inside the async world!");
///     });
/// }
/// ```
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