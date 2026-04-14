use std::time::Instant;
use taiga::net::{AsyncTcpListener, AsyncTcpStream};
use std::io::{Read, Write};
use std::net::SocketAddr;

fn main() {
    println!("Taiga vs Standard Threaded Echo Benchmark");
    println!("-----------------------------------------");

    let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
    let num_iterations = 1000;

    // 1. Taiga Benchmark
    let taiga_start = Instant::now();
    taiga::block_on(async move {
        let listener = AsyncTcpListener::bind(addr).unwrap();
        
        taiga::spawn(async move {
            for _ in 0..num_iterations {
                if let Ok((stream, _)) = listener.accept().await {
                    taiga::spawn(async move {
                        let mut buf = [0u8; 1024];
                        if let Ok(n) = stream.read(&mut buf).await {
                            let _ = stream.write(&buf[..n]).await;
                        }
                    });
                }
            }
        });

        for _ in 0..num_iterations {
            let stream = AsyncTcpStream::connect(addr).await.unwrap();
            stream.write(b"hello taiga").await.unwrap();
            let mut buf = [0u8; 11];
            stream.read(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello taiga");
        }
    });
    let taiga_duration = taiga_start.elapsed();
    println!("Taiga (Async): {:?}", taiga_duration);

    // 2. Standard Threads Benchmark
    let std_start = Instant::now();
    let std_addr: SocketAddr = "127.0.0.1:9998".parse().unwrap();
    let std_listener = std::net::TcpListener::bind(std_addr).unwrap();
    std::thread::spawn(move || {
        for _ in 0..num_iterations {
            if let Ok((mut stream, _)) = std_listener.accept() {
                std::thread::spawn(move || {
                    let mut buf = [0u8; 1024];
                    if let Ok(n) = stream.read(&mut buf) {
                        let _ = stream.write_all(&buf[..n]);
                    }
                });
            }
        }
    });

    for _ in 0..num_iterations {
        let mut stream = std::net::TcpStream::connect(std_addr).unwrap();
        stream.write_all(b"hello std").unwrap();
        let mut buf = [0u8; 9];
        stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello std");
    }
    let std_duration = std_start.elapsed();
    println!("Std (Threads): {:?}", std_duration);

    println!("\nConclusion:");
    if taiga_duration < std_duration {
        println!("Taiga is {:.2}x faster than standard threads!", std_duration.as_secs_f64() / taiga_duration.as_secs_f64());
    } else {
        println!("Standard threads were slightly faster (expected for low concurrency/localhost overhead)");
    }
}
