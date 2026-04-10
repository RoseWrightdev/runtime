use runtime::executor::Runtime;
use std::future::Future;
use std::pin::Pin;

#[test]
fn test_reuse_join_bug() {
    let runtime = Runtime::new();

    fn my_task(val: usize, sleep: bool) -> Pin<Box<dyn Future<Output = usize> + Send>> {
        Box::pin(async move {
            if sleep {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            val
        })
    }

    // Spawn 130 tasks and drop them so they get recycled.
    // The worker thread will execute them, and recycle them to its local pool.
    // The first 128 will go to its local pool, and the next 2 will go to the GLOBAL pool!
    for _ in 0..130 {
        let handle1 = runtime.spawn(my_task(42, false));
        drop(handle1);
    }

    // Give it time to execute and recycle
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Now spawn another task from the main thread.
    // Thread-local pool is empty. Global pool has 2!
    // It will pop from the global pool!
    let handle2 = runtime.spawn(my_task(84, true));

    // Wait, handle2's JoinHandle will immediately see JOIN_STATE_READY from the old execution
    let res = runtime.block_on(handle2).unwrap();
    assert_eq!(res, 84, "Bug reproduced: it returned the old task's value!");
}
