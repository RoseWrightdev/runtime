//! The core executor and task management system for Taiga.
//! 
//! This module contains the primary components of the asynchronous runtime:
//! - [`Runtime`]: The top-level orchestrator and entry point.
//! - [`Scheduler`]: The work-stealing task distribution system.
//! - [`Worker`]: The execution loop that runs on each thread.
//! - [`Reactor`]: The I/O and timer event driver.
//! - [`Task`]: The unit of execution, wrapping a type-erased future.

pub mod runtime;    
pub mod scheduler;  
pub mod worker;     
pub mod task;       
pub mod context;
pub mod handle;
pub mod reactor;
pub mod join_handle;

// Re-export only what the rest of the crate needs
pub use self::runtime::Runtime;
pub use self::scheduler::Scheduler;
pub use self::reactor::{Reactor, Registration};
pub use self::handle::Handle;
pub use self::context::CONTEXT;
pub use self::join_handle::JoinHandle;

pub use self::worker::Worker;
pub use self::task::{Task, RawFuture};