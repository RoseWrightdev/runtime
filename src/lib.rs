pub mod core;
pub mod net;
pub mod time;

use crate::core::runtime::context::Context as RuntimeContext;
pub use crate::core::runtime::runtime::Runtime;

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
    use std::sync::{Arc, Mutex, mpsc};

    #[test]
    fn test_block_on_basic() {
        let result = block_on(async { 42 });
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

    #[test]
    fn test_panic_survivability() {
        let (tx, rx) = mpsc::channel();

        block_on(async move {
            spawn(async {
                panic!("intentional panic");
            });

            spawn(async move {
                tx.send(Ok::<_, ()>(())).unwrap();
            });
        });

        assert!(rx.recv().is_ok());
    }

    #[test]
    fn test_deep_recursion_nesting() {
        fn recursive_spawn(n: usize, tx: mpsc::Sender<()>) {
            if n == 0 {
                tx.send(()).unwrap();
                return;
            }
            spawn(async move {
                recursive_spawn(n - 1, tx);
            });
        }

        let (tx, rx) = mpsc::channel();
        block_on(async move {
            recursive_spawn(100, tx);
        });

        assert!(rx.recv().is_ok());
    }

    #[test]
    fn test_stress_concurrency_heavy() {
        let (tx, rx) = mpsc::channel();
        let num_tasks = 10_000;

        block_on(async move {
            for _ in 0..num_tasks {
                let tx = tx.clone();
                spawn(async move {
                    tx.send(1).unwrap();
                });
            }
        });

        let count: i32 = rx.iter().take(num_tasks).sum();
        assert_eq!(count, num_tasks as i32);
    }

    #[test]
    fn test_shutdown_signaling() {
        let rt = Runtime::new();
        assert!(!rt.is_shutdown());
        rt.shutdown();
        assert!(rt.is_shutdown());
    }

    #[test]
    fn test_lifo_slot_priority() {
        let execution_order = Arc::new(Mutex::new(Vec::new()));

        let order_clone = execution_order.clone();
        // Use only 1 worker to ensure deterministic LIFO order without interference from stealing
        Runtime::with_workers(1).block_on(async move {
            let order = order_clone.clone();

            // Spawn Task C (should go to LIFO slot)
            let order_c = order.clone();
            spawn(async move {
                order_c.lock().unwrap().push('C');
            });

            // Task A is running right now.
            // It spawns Task B. Since LIFO works, B should replace C in the slot.
            // C should be pushed to the local queue.
            let order_b = order.clone();
            spawn(async move {
                order_b.lock().unwrap().push('B');
            });

            order.lock().unwrap().push('A');
        });
        
        // Wait for workers to finish B and C (they were queued during A)
        // Since we have a single worker and we know the order, we can just wait 
        // for the vector to reach size 3.
        let mut attempts = 0;
        loop {
            {
                let order = execution_order.lock().unwrap();
                if order.len() >= 3 {
                    break;
                }
            }
            std::thread::yield_now();
            attempts += 1;
            if attempts > 1000 {
                panic!("Tasks B and C failed to execute in time");
            }
        }

        let final_order = execution_order.lock().unwrap();
        // B (slot) should run before C (queue)
        assert_eq!(
            *final_order,
            vec!['A', 'B', 'C'],
            "High priority task B should have run before C"
        );
    }
}
