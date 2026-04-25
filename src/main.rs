use comfy_table::presets::UTF8_FULL;
use comfy_table::*;
use hdrhistogram::Histogram;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener as TokioListener, TcpStream as TokioStream};
use std::os::unix::io::{AsRawFd, FromRawFd};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let concurrency = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
    let total_messages = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10000);
    let payload_size = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(65536);
    let runs = args
        .windows(2)
        .find(|w| w[0] == "--runs" || w[0] == "-r")
        .and_then(|w| w[1].parse::<usize>().ok())
        .unwrap_or(1);

    println!("\n\n");
    println!(
        "Payload: {} bytes | Concurrency: {} | Total: {} msgs | Runs: {}\n",
        payload_size, concurrency, total_messages, runs
    );

    let mut custom_run_results = Vec::new();
    let mut tokio_run_results = Vec::new();

    for i in 1..=runs {
        if runs > 1 {
            println!("\x1b[1;36m▶️ Run {}/{}\x1b[0m", i, runs);
        }

        // Run Custom Runtime Benchmark
        if runs == 1 {
            println!("\x1b[1;33m[1/2] Benchmarking Custom Runtime...\x1b[0m");
        }
        custom_run_results.push(run_custom_benchmark(concurrency, total_messages, payload_size));
        
        // Run Tokio Runtime Benchmark
        if runs == 1 {
            println!("\x1b[1;33m[2/2] Benchmarking Tokio Runtime (Multi-thread)...\x1b[0m");
        }
        tokio_run_results.push(run_tokio_benchmark(concurrency, total_messages, payload_size));

        if runs > 1 {
            println!("\x1b[1;32m   ✓ Finished run {}\x1b[0m", i);
        }
    }

    let custom_results = aggregate_median(custom_run_results);
    let tokio_results = aggregate_median(tokio_run_results);

    println!("\n\x1b[1;32m✅ All runs complete. Reporting Median Values:\x1b[0m\n");

    // Display Results
    print_results_table(custom_results, tokio_results);
}

#[derive(Clone, Copy)]
struct BenchResults {
    duration: Duration,
    throughput: f64,
    msg_rate: f64,
    avg: f64,
    p50: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

fn aggregate_median(mut results: Vec<BenchResults>) -> BenchResults {
    if results.is_empty() {
        panic!("No results to aggregate");
    }
    if results.len() == 1 {
        return results.pop().unwrap();
    }

    let mut durations: Vec<f64> = results.iter().map(|r| r.duration.as_secs_f64()).collect();
    let mut throughputs: Vec<f64> = results.iter().map(|r| r.throughput).collect();
    let mut msg_rates: Vec<f64> = results.iter().map(|r| r.msg_rate).collect();
    let mut avgs: Vec<f64> = results.iter().map(|r| r.avg).collect();
    let mut p50s: Vec<u64> = results.iter().map(|r| r.p50).collect();
    let mut p95s: Vec<u64> = results.iter().map(|r| r.p95).collect();
    let mut p99s: Vec<u64> = results.iter().map(|r| r.p99).collect();
    let mut maxs: Vec<u64> = results.iter().map(|r| r.max).collect();

    fn median_f64(values: &mut [f64]) -> f64 {
        values.sort_by(|a, b| a.partial_cmp(b).expect("NaN in results"));
        let mid = values.len() / 2;
        if values.len().is_multiple_of(2) {
            (values[mid - 1] + values[mid]) / 2.0
        } else {
            values[mid]
        }
    }

    fn median_u64(values: &mut [u64]) -> u64 {
        values.sort();
        let mid = values.len() / 2;
        if values.len().is_multiple_of(2) {
            (values[mid - 1] + values[mid]) / 2
        } else {
            values[mid]
        }
    }

    BenchResults {
        duration: Duration::from_secs_f64(median_f64(&mut durations)),
        throughput: median_f64(&mut throughputs),
        msg_rate: median_f64(&mut msg_rates),
        avg: median_f64(&mut avgs),
        p50: median_u64(&mut p50s),
        p95: median_u64(&mut p95s),
        p99: median_u64(&mut p99s),
        max: median_u64(&mut maxs),
    }
}

fn run_custom_benchmark(concurrency: usize, total_messages: usize, payload_size: usize) -> BenchResults {
    let rt = runtime::Runtime::new();
    let start = Instant::now();
    let mut total_hist = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();

    let histograms = rt.block_on(async move {
        let listener = runtime::AsyncTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());

        // Server
        runtime::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                runtime::spawn(async move {
                    let mut buf = vec![0u8; payload_size];
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
                });
            }
        });

        // Clients
        let messages_per_client = total_messages / concurrency;
        let mut handlers = Vec::new();

        for _ in 0..concurrency {
            let addr = addr.clone();
            let (tx, rx) = futures::channel::oneshot::channel();
            runtime::spawn(async move {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();
                let mut stream = None;
                for _ in 0..100 {
                    if let Ok(s) = runtime::AsyncTcpStream::connect(&addr).await {
                        stream = Some(s);
                        break;
                    }
                    runtime::sleep(Duration::from_millis(5)).await;
                }

                if let Some(stream) = stream {
                    let payload = vec![0u8; payload_size];
                    let mut buf = vec![0u8; payload_size];
                    for _ in 0..messages_per_client {
                        let start_msg = Instant::now();
                        if stream.write_all(&payload).await.is_err() {
                            break;
                        }
                        let mut read = 0;
                        while read < payload_size {
                            match stream.read(&mut buf[read..]).await {
                                Ok(n) if n > 0 => read += n,
                                _ => break,
                            }
                        }
                        let _ = hist.record(start_msg.elapsed().as_nanos() as u64);
                    }
                }
                let _ = tx.send(hist);
            });
            handlers.push(rx);
        }

        let mut hists = Vec::new();
        for rx in handlers {
            if let Ok(h) = rx.await {
                hists.push(h);
            }
        }
        hists
    });

    for h in histograms {
        total_hist.add(h).unwrap();
    }

    let duration = start.elapsed();
    let throughput = (total_messages * payload_size) as f64 / duration.as_secs_f64();

    BenchResults {
        duration,
        throughput: throughput / (1024.0 * 1024.0), // MiB/s
        msg_rate: total_messages as f64 / duration.as_secs_f64(),
        avg: total_hist.mean() / 1_000_000.0,
        p50: total_hist.value_at_quantile(0.5) / 1000,
        p95: total_hist.value_at_quantile(0.95) / 1000,
        p99: total_hist.value_at_quantile(0.99) / 1000,
        max: total_hist.max() / 1000,
    }
}

fn run_tokio_benchmark(concurrency: usize, total_messages: usize, payload_size: usize) -> BenchResults {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let start = Instant::now();
    let mut total_hist = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();

    let histograms = rt.block_on(async {
        let listener = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0u8; payload_size];
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
                });
            }
        });

        // Clients
        let messages_per_client = total_messages / concurrency;
        let mut handles = Vec::new();

        for _ in 0..concurrency {
            let handle = tokio::spawn(async move {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();
                if let Ok(mut stream) = TokioStream::connect(addr).await {
                    // Set linger(0) to mitigate port exhaustion
                    unsafe {
                        let socket = socket2::Socket::from_raw_fd(stream.as_raw_fd());
                        let _ = socket.set_linger(Some(Duration::ZERO));
                        std::mem::forget(socket);
                    }

                    let payload = vec![0u8; payload_size];
                    let mut buf = vec![0u8; payload_size];
                    for _ in 0..messages_per_client {
                        let start_msg = Instant::now();
                        if stream.write_all(&payload).await.is_err() {
                            break;
                        }
                        let mut read = 0;
                        while read < payload_size {
                            match stream.read(&mut buf[read..]).await {
                                Ok(n) if n > 0 => read += n,
                                _ => break,
                            }
                        }
                        let _ = hist.record(start_msg.elapsed().as_nanos() as u64);
                    }
                }
                hist
            });
            handles.push(handle);
        }

        let mut hists = Vec::new();
        for handle in handles {
            if let Ok(h) = handle.await {
                hists.push(h);
            }
        }
        hists
    });

    for h in histograms {
        total_hist.add(h).unwrap();
    }

    let duration = start.elapsed();
    let throughput = (total_messages * payload_size) as f64 / duration.as_secs_f64();

    BenchResults {
        duration,
        throughput: throughput / (1024.0 * 1024.0), // MiB/s
        msg_rate: total_messages as f64 / duration.as_secs_f64(),
        avg: total_hist.mean() / 1_000_000.0,
        p50: total_hist.value_at_quantile(0.5) / 1000,
        p95: total_hist.value_at_quantile(0.95) / 1000,
        p99: total_hist.value_at_quantile(0.99) / 1000,
        max: total_hist.max() / 1000,
    }
}

fn print_results_table(custom: BenchResults, tokio: BenchResults) {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new("Metric").add_attribute(Attribute::Bold),
        Cell::new("Custom Runtime")
            .add_attribute(Attribute::Bold)
            .set_alignment(CellAlignment::Center),
        Cell::new("Tokio Runtime")
            .add_attribute(Attribute::Bold)
            .set_alignment(CellAlignment::Center),
        Cell::new("Rel. Stats")
            .add_attribute(Attribute::Bold)
            .set_alignment(CellAlignment::Center),
    ]);

    // Total Time
    table.add_row(vec![
        Cell::new("Total Time"),
        Cell::new(format!("{:.2?}", custom.duration)),
        Cell::new(format!("{:.2?}", tokio.duration)),
        Cell::new("-"),
    ]);

    // Throughput
    let speedup = custom.throughput / tokio.throughput;
    let custom_tp_color = if speedup >= 0.95 {
        Color::Green
    } else {
        Color::Reset
    };
    let tokio_tp_color = if speedup < 0.95 {
        Color::Green
    } else {
        Color::Reset
    };

    table.add_row(vec![
        Cell::new("Throughput"),
        Cell::new(format!("{:.2} MiB/s", custom.throughput))
            .fg(custom_tp_color)
            .add_attribute(Attribute::Bold),
        Cell::new(format!("{:.2} MiB/s", tokio.throughput))
            .fg(tokio_tp_color)
            .add_attribute(Attribute::Bold),
        Cell::new(format!("{:.2}x", speedup)).fg(if speedup >= 0.95 {
            Color::Green
        } else {
            Color::Red
        }),
    ]);

    // Message Rate
    let msg_speedup = custom.msg_rate / tokio.msg_rate;
    table.add_row(vec![
        Cell::new("Message Rate"),
        Cell::new(format!("{:.0} msg/s", custom.msg_rate)),
        Cell::new(format!("{:.0} msg/s", tokio.msg_rate)),
        Cell::new(format!("{:.2}x", msg_speedup)).fg(if msg_speedup >= 0.95 {
            Color::Green
        } else {
            Color::Red
        }),
    ]);

    // Latency Avg
    let custom_avg_color = if custom.avg <= tokio.avg * 1.05 {
        Color::Yellow
    } else {
        Color::Reset
    };
    let tokio_avg_color = if tokio.avg < custom.avg / 1.05 {
        Color::Yellow
    } else {
        Color::Reset
    };

    let lat_speedup = tokio.avg / custom.avg;
    table.add_row(vec![
        Cell::new("Avg Latency"),
        Cell::new(format!("{:.3} ms", custom.avg)).fg(custom_avg_color),
        Cell::new(format!("{:.3} ms", tokio.avg)).fg(tokio_avg_color),
        Cell::new(format!("{:.2}x", lat_speedup)).fg(if lat_speedup >= 0.95 {
            Color::Green
        } else {
            Color::Red
        }),
    ]);

    // P50, P95, P99
    let latencies = [
        ("P50 Latency", custom.p50, tokio.p50),
        ("P95 Latency", custom.p95, tokio.p95),
        ("P99 Latency", custom.p99, tokio.p99),
    ];

    for (label, c_val, t_val) in latencies {
        let c_color = if c_val <= (t_val as f64 * 1.05) as u64 {
            Color::Cyan
        } else {
            Color::Reset
        };
        let t_color = if t_val < (c_val as f64 / 1.05) as u64 {
            Color::Cyan
        } else {
            Color::Reset
        };
        let rel = t_val as f64 / c_val as f64;
        table.add_row(vec![
            Cell::new(label),
            Cell::new(format!("{} µs", c_val)).fg(c_color),
            Cell::new(format!("{} µs", t_val)).fg(t_color),
            Cell::new(format!("{:.2}x", rel)).fg(if rel >= 0.95 {
                Color::Green
            } else {
                Color::Red
            }),
        ]);
    }

    // Max Latency
    let max_rel = tokio.max as f64 / custom.max as f64;
    table.add_row(vec![
        Cell::new("Max Latency"),
        Cell::new(format!("{} µs", custom.max)).fg(if custom.max <= tokio.max {
            Color::Red
        } else {
            Color::Reset
        }),
        Cell::new(format!("{} µs", tokio.max)).fg(if tokio.max < custom.max {
            Color::Red
        } else {
            Color::Reset
        }),
        Cell::new(format!("{:.2}x", max_rel)).fg(if max_rel >= 0.95 {
            Color::Green
        } else {
            Color::Red
        }),
    ]);

    println!("{}", table);

    let final_speedup = custom.throughput / tokio.throughput;
    println!("\nCustom Runtime is {:.2}x the speed of Tokio", final_speedup);
}
