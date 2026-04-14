use std::future::Future;
use std::time::Duration;
use crate::block_on;

/// Runs a future to completion on a new temporary runtime, but panics if it exceeds the timeout.
/// This is used in tests to detect stalls in the runtime.
pub fn block_on_timeout<F>(future: F, timeout: Duration) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    
    std::thread::spawn(move || {
        let result = block_on(future);
        let _ = tx.send(result);
    });

    rx.recv_timeout(timeout).expect("Test stalled!")
}
