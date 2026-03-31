use criterion::{criterion_group, criterion_main, Criterion};
use std::time::Instant;

const PAYLOAD_SIZE: usize = 64 * 1024; // 64KB

fn custom_runtime_echo(c: &mut Criterion) {
    let mut group = c.benchmark_group("TCP Echo");
    group.throughput(criterion::Throughput::Bytes(PAYLOAD_SIZE as u64));

    let rt = runtime::Runtime::new();

    group.bench_function("Custom Runtime", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async move {
                let start = Instant::now();
                for _ in 0..iters {
                    let listener = runtime::AsyncTcpListener::bind("127.0.0.1:0").unwrap();
                    let addr = listener.local_addr().unwrap();

                    let server = runtime::spawn(async move {
                        if let Ok((stream, _)) = listener.accept().await {
                            let mut buf = vec![0u8; PAYLOAD_SIZE];
                            loop {
                                match stream.read(&mut buf).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => {
                                        if stream.write_all(&buf[..n]).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    });

                    let client = runtime::spawn(async move {
                        let stream = runtime::AsyncTcpStream::connect(addr).await.unwrap();
                        let mut buf = vec![0u8; PAYLOAD_SIZE];
                        let mut recv_buf = vec![0u8; PAYLOAD_SIZE];
                        
                        stream.write_all(&buf).await.unwrap();
                        let mut read = 0;
                        while read < PAYLOAD_SIZE {
                            match stream.read(&mut recv_buf[read..]).await {
                                Ok(n) if n > 0 => read += n,
                                _ => break,
                            }
                        }
                    });

                    let _ = client.await;
                    let _ = server.await;
                }
                start.elapsed()
            })
        });
    });
    group.finish();
}

fn tokio_runtime_echo(c: &mut Criterion) {
    let mut group = c.benchmark_group("TCP Echo");
    group.throughput(criterion::Throughput::Bytes(PAYLOAD_SIZE as u64));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    group.bench_function("Tokio Runtime", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async move {
                let start = Instant::now();
                for _ in 0..iters {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let addr = listener.local_addr().unwrap();

                    let server = tokio::spawn(async move {
                        if let Ok((mut stream, _)) = listener.accept().await {
                            let mut buf = vec![0u8; PAYLOAD_SIZE];
                            loop {
                                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                                match stream.read(&mut buf).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => {
                                        if stream.write_all(&buf[..n]).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    });

                    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
                    let buf = vec![0u8; PAYLOAD_SIZE];
                    let mut recv_buf = vec![0u8; PAYLOAD_SIZE];
                    
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    stream.write_all(&buf).await.unwrap();
                    let mut read = 0;
                    while read < PAYLOAD_SIZE {
                        match stream.read(&mut recv_buf[read..]).await {
                            Ok(n) if n > 0 => read += n,
                            _ => break,
                        }
                    }
                    let _ = server.await;
                }
                start.elapsed()
            })
        });
    });
    group.finish();
}

criterion_group!(benches, custom_runtime_echo, tokio_runtime_echo);
criterion_main!(benches);
