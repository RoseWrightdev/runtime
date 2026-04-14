pub mod core;
pub mod net;
pub mod time;
pub mod utils;

use crate::core::runtime::context::Context as RuntimeContext;
pub use crate::core::runtime::runtime::Runtime;
pub use crate::core::scheduler::join::JoinHandle;

use std::any::Any;
use std::future::Future;

pub fn spawn<F, T>(future: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Any + Send + 'static,
{
    RuntimeContext::current()
        .expect("Taiga: spawn called outside of a runtime context")
        .spawn_internal(future)
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
        assert_eq!(
            *final_order,
            vec!['A', 'B', 'C'],
            "High priority task B should have run before C"
        );
    }

    #[test]
    fn test_join_handle_success() {
        let result = block_on(async {
            let handle = spawn(async { 42 });
            handle.await.unwrap()
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn test_join_handle_panic() {
        let result = block_on(async {
            let handle = spawn(async {
                panic!("intentional panic");
                #[allow(unreachable_code)]
                42
            });
            handle.await
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_tcp_echo_server() {
        use crate::net::{AsyncTcpStream, AsyncTcpListener};

        let addr = "127.0.0.1:0".parse().unwrap();
        let num_clients = 10;
        let message = b"echo this";

        crate::utils::block_on_timeout(async move {
            let listener = AsyncTcpListener::bind(addr).unwrap();
            let addr = listener.local_addr().unwrap();

            // Server task
            spawn(async move {
                for _ in 0..num_clients {
                    let (async_stream, _) = listener.accept().await.unwrap();
                    
                    spawn(async move {
                        let mut buf = [0u8; 1024];
                        loop {
                            let n = std::future::poll_fn(|cx| async_stream.poll_read(cx, &mut buf)).await.unwrap();
                            if n == 0 { break; }
                            
                            let mut written = 0;
                            while written < n {
                                let w = std::future::poll_fn(|cx| async_stream.poll_write(cx, &buf[written..n])).await.unwrap();
                                written += w;
                            }
                        }
                    });
                }
            });

            // Client tasks
            let mut handles = Vec::new();
            for _ in 0..num_clients {
                handles.push(spawn(async move {
                    let client = AsyncTcpStream::connect(addr).await.unwrap();
                    
                    // Send
                    let mut written = 0;
                    while written < message.len() {
                        let n = std::future::poll_fn(|cx| client.poll_write(cx, &message[written..])).await.unwrap();
                        written += n;
                    }

                    // Read echo
                    let mut read_buf = vec![0u8; message.len()];
                    let mut read_bytes = 0;
                    while read_bytes < message.len() {
                        let n = std::future::poll_fn(|cx| client.poll_read(cx, &mut read_buf[read_bytes..])).await.unwrap();
                        if n == 0 { break; }
                        read_bytes += n;
                    }
                    assert_eq!(&read_buf, message);
                }));
            }

            for h in handles {
                h.await.unwrap();
            }
        }, std::time::Duration::from_secs(5));
    }

    #[test]
    fn test_stress_panic_propagation() {
        let num_tasks = 100;
        
        crate::utils::block_on_timeout(async move {
            let mut handles = Vec::new();
            
            for i in 0..num_tasks {
                handles.push(spawn(async move {
                    if i % 2 == 0 {
                        panic!("intentional panic {}", i);
                    }
                    i
                }));
            }
            
            let mut success_count = 0;
            let mut panic_count = 0;
            
            for h in handles {
                match h.await {
                    Ok(val) => {
                        assert!(val % 2 != 0);
                        success_count += 1;
                    }
                    Err(_) => {
                        panic_count += 1;
                    }
                }
            }
            
            assert_eq!(success_count, 50);
            assert_eq!(panic_count, 50);
        }, std::time::Duration::from_secs(10));
    }

    #[test]
    fn test_task_nesting() {
        crate::utils::block_on_timeout(async {
            // Spawn a task that itself spawns another task
            let res = crate::spawn(async {
                let inner_res = crate::spawn(async {
                    100
                }).await.unwrap();
                inner_res + 23
            }).await.unwrap();
            
            assert_eq!(res, 123);
        }, std::time::Duration::from_secs(5));
    }

    #[test]
    #[should_panic(expected = "Taiga: spawn called outside of a runtime context")]
    fn test_external_spawn_panic() {
        // Calling spawn outside of a block_on or any runtime thread should panic
        let _ = crate::spawn(async {});
    }
}
