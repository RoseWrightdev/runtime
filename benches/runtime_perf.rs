use criterion::{criterion_group, criterion_main, Criterion};
use taiga::core::runtime::runtime::Runtime as TaigaRuntime;
use futures::future::{BoxFuture, FutureExt};

// --- Task Workloads ---

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

// --- Benchmark Functions ---

fn bench_fib(c: &mut Criterion) {
    let mut group = c.benchmark_group("Recursive Fibonacci (Depth 15)");
    let n = 15;

    group.bench_function("Taiga", |b| {
        let rt = TaigaRuntime::new();
        b.iter(|| rt.block_on(taiga_fib(n)));
    });

    group.bench_function("Tokio", |b| {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        b.iter(|| rt.block_on(tokio_fib(n)));
    });

    group.finish();
}

fn bench_burst(c: &mut Criterion) {
    let mut group = c.benchmark_group("Task Burst (10,000 Tasks)");
    let count = 10_000;

    group.bench_function("Taiga", |b| {
        let rt = TaigaRuntime::new();
        b.iter(|| rt.block_on(taiga_burst(count)));
    });

    group.bench_function("Tokio", |b| {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        b.iter(|| rt.block_on(tokio_burst(count)));
    });

    group.finish();
}

criterion_group!(benches, bench_fib, bench_burst);
criterion_main!(benches);
