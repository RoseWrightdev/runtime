//! # The Taiga Executor
//! 
//! The executor is the core of the Taiga runtime. It is responsible for 
//! managing the lifecycle of tasks and distributing them across multiple 
//! CPU cores.
//! 
//! ## System Architecture
//! 
//! 1.  **Task Submission**: When `spawn(future)` is called, the [`Scheduler`] 
//!     encapsulates the future into a type-erased [`Task`].
//! 2.  **Load Balancing**: The task is enqueued. Taiga utilizes a 
//!     **work-stealing** scheduler where each thread manages a local deque, 
//!     migrating tasks between threads to maintain high CPU utilization.
//! 3.  **Execution Loop**: Each worker thread runs a [`Worker`] loop that 
//!     polls tasks from the local deque, the global injector, or by stealing 
//!     from peers.
//! 4.  **Event Registration**: If a task returns `Poll::Pending`, it may 
//!     register interest in I/O or timer events with the [`Reactor`].
//! 5.  **Event Dispatch**: The Reactor monitors the OS kernel for events 
//!     and re-schedules the associated Task once the dependency is satisfied.
//! 
//! ## Core Components
//! - [`Runtime`]: Manages the thread pool lifecycle and system initialization.
//! - [`Scheduler`]: Orchestrates task distribution and manages object-pooled resource recycling.
//! - [`Worker`]: Implements the per-thread execution loop and task polling logic.
//! - [`Reactor`]: Interfaces with OS primitives (epoll/kqueue) to provide asynchronous I/O and timers.
//! - [`Task`]: Represents a unit of execution containing a future, state, and result storage.

pub mod runtime;    
pub mod scheduler;  
pub mod worker;     
pub mod task;       
pub mod context;
pub mod handle;
pub mod reactor;
pub mod join_handle;
pub(crate) mod stats;


// Re-export only what the rest of the crate needs
pub use self::runtime::Runtime;
pub use self::scheduler::Scheduler;
pub use self::reactor::{Reactor, Registration};
pub use self::handle::Handle;
pub use self::context::CONTEXT;
pub use self::join_handle::JoinHandle;

pub use self::worker::Worker;
pub use self::task::{Task, RawFuture};