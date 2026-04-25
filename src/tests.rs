use crate::{Runtime, spawn, sleep};
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn test_spawn_result() {
    let runtime = Runtime::new();
    let result = runtime.block_on(async {
        let handle = spawn(async {
            42
        });
        handle.await.unwrap()
    });
    assert_eq!(result, 42);
}

#[test]
fn test_panic_propagation() {
    let runtime = Runtime::new();
    runtime.block_on(async {
        let handle = spawn(async {
            panic!("Task panicked!");
        });
        let res = handle.await;
        assert!(res.is_err());
    });
}

#[test]
fn test_work_stealing() {
    let runtime = Runtime::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let num_tasks = 1000;
    
    let c_clone = counter.clone();
    runtime.block_on(async move {
        let mut handles = Vec::with_capacity(num_tasks);
        for _ in 0..num_tasks {
            let c = c_clone.clone();
            handles.push(spawn(async move {
                c.fetch_add(1, Ordering::SeqCst);
            }));
        }
        
        for h in handles {
            h.await.unwrap();
        }
    });
    
    assert_eq!(counter.load(Ordering::SeqCst), num_tasks);
}

#[test]
fn test_timer_sleep() {
    let runtime = Runtime::new();
    runtime.block_on(async {
        let start = Instant::now();
        let duration = Duration::from_millis(100);
        sleep(duration).await;
        let elapsed = start.elapsed();
        // Allow some slack for scheduler latency
        assert!(elapsed >= duration - Duration::from_millis(10));
    });
}

#[test]
fn test_nested_spawn() {
    let runtime = Runtime::new();
    runtime.block_on(async {
        let h1 = spawn(async {
            let h2 = spawn(async {
                100
            });
            h2.await.unwrap()
        });
        assert_eq!(h1.await.unwrap(), 100);
    });
}

#[test]
fn test_panic_hook_delegation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::panic;

    let called = Arc::new(AtomicBool::new(false));
    let called_clone = called.clone();

    // Set a custom hook that we expect to be called
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        called_clone.store(true, Ordering::SeqCst);
        default_hook(info);
    }));

    let runtime = crate::Runtime::new();
    
    // This task panics. The runtime hook should delegate to our custom hook.
    let _ = runtime.block_on(async {
        let handle = crate::spawn(async {
            panic!("Test panic delegation");
        });
        handle.await
    });

    // Clean up hook
    let _ = panic::take_hook();

    assert!(called.load(Ordering::SeqCst), "Panic hook was not called!");
}

#[test]
fn test_join_handle_concurrency_stress() {
    let runtime = crate::Runtime::new();
    
    for i in 0..5000 {
        let handle = runtime.spawn(async move {
            i
        });

        let res = runtime.block_on(async move {
            handle.await.unwrap()
        });
        
        assert_eq!(res, i);
    }
}

#[test]
fn test_runtime_shutdown() {
    let runtime = Runtime::new();
    // Dropping the runtime should join all threads and exit cleanly.
    drop(runtime);
}
