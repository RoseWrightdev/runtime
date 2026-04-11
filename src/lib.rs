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
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn test_block_on_basic() {
        let result = block_on(async {
            42
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn test_spawn_and_collect() {
        let (tx, rx) = mpsc::channel();
        
        block_on(async move {
            spawn(async move {
                tx.send(100).unwrap();
            });
        });

        assert_eq!(rx.recv().unwrap(), 100);
    }

    #[test]
    fn test_multi_spawn() {
        let (tx, rx) = mpsc::channel();
        
        block_on(async move {
            for i in 0..10 {
                let tx = tx.clone();
                spawn(async move {
                    tx.send(i).unwrap();
                });
            }
        });

        let mut results: Vec<i32> = rx.iter().take(10).collect();
        results.sort();
        let expected: Vec<i32> = (0..10).collect();
        assert_eq!(results, expected);
    }
}