use taiga::core::runtime::runtime::Runtime as TaigaRuntime;
use tokio::runtime::Runtime as TokioRuntime;
use futures::future::{BoxFuture, FutureExt};
use std::time::Instant;

fn taiga_fib(n: u32) -> BoxFuture<'static, u32> {
    async move {
        if n <= 1 {
            return n;
        }

        let f1 = taiga::spawn(taiga_fib(n - 1));
        let f2 = taiga::spawn(taiga_fib(n - 2));

        f1.await.unwrap() + f2.await.unwrap()
    }.boxed()
}

fn tokio_fib(n: u32) -> BoxFuture<'static, u32> {
    async move {
        if n <= 1 {
            return n;
        }

        let f1 = tokio::spawn(tokio_fib(n - 1));
        let f2 = tokio::spawn(tokio_fib(n - 2));

        f1.await.unwrap() + f2.await.unwrap()
    }.boxed()
}

async fn taiga_burst(count: usize) {
    let mut handles = Vec::with_capacity(count);
    for _ in 0..count {
        handles.push(taiga::spawn(async {}));
    }
    for h in handles {
        let _ = h.await;
    }
}

async fn tokio_burst(count: usize) {
    let mut handles = Vec::with_capacity(count);
    for _ in 0..count {
        handles.push(tokio::spawn(async {}));
    }
    for h in handles {
        let _ = h.await;
    }
}

fn main() {
    let fib_n = 15;
    let burst_count = 10_000;

    println!("--- Benchmarking Recursive Fibonacci (Depth {}) ---", fib_n);

    // Taiga Fib
    {
        let rt = TaigaRuntime::new();
        let start = Instant::now();
        let res = rt.block_on(taiga_fib(fib_n));
        let duration = start.elapsed();
        println!("Taiga Fib:  {:?} (result: {})", duration, res);
    }

    // Tokio Fib
    {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let start = Instant::now();
        let res = rt.block_on(tokio_fib(fib_n));
        let duration = start.elapsed();
        println!("Tokio Fib:  {:?} (result: {})", duration, res);
    }

    println!("\n--- Benchmarking Task Burst ({} Tasks) ---", burst_count);

    // Taiga Burst
    {
        let rt = TaigaRuntime::new();
        let start = Instant::now();
        rt.block_on(taiga_burst(burst_count));
        let duration = start.elapsed();
        println!("Taiga Burst: {:?}", duration);
    }

    // Tokio Burst
    {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let start = Instant::now();
        rt.block_on(tokio_burst(burst_count));
        let duration = start.elapsed();
        println!("Tokio Burst: {:?}", duration);
    }
}
