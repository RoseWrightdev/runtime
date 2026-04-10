use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::{Duration, Instant};
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::sync::{Arc, Mutex};
use hdrhistogram::Histogram;
use comfy_table::presets::UTF8_FULL;
use comfy_table::*;
use std::fs::OpenOptions;
use std::io::{Write, BufRead, BufReader};
use std::collections::HashMap;

const PAYLOAD_SIZE: usize = 1024;
const TOTAL_MESSAGES: usize = 50_000;
const CONCURRENCIES: [usize; 14] = [1, 10, 50, 100, 250, 500, 750, 1000, 2000, 3000, 4000, 5000, 7500, 10_000];

/// Sets SO_LINGER(0) to mitigate port exhaustion during high-concurrency benchmarks.
fn set_linger_zero<S: AsRawFd>(stream: &S) {
    unsafe {
        let socket = socket2::Socket::from_raw_fd(stream.as_raw_fd());
        let _ = socket.set_linger(Some(Duration::ZERO));
        std::mem::forget(socket);
    }
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

    fn median_f64(values: &mut Vec<f64>) -> f64 {
        values.sort_by(|a, b| a.partial_cmp(b).expect("NaN in results"));
        let mid = values.len() / 2;
        if values.len() % 2 == 0 {
            (values[mid - 1] + values[mid]) / 2.0
        } else {
            values[mid]
        }
    }

    fn median_u64(values: &mut Vec<u64>) -> u64 {
        values.sort();
        let mid = values.len() / 2;
        if values.len() % 2 == 0 {
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

fn print_results_table(title: &str, concurrency: usize, iters: u64, results: BenchResults, baseline: Option<BenchResults>) {
    println!("\n\x1b[1;32m📊 Summary for {} (Concurrency: {}, Total Iters: {})\x1b[0m", title, concurrency, iters);
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    
    let mut header = vec![
        Cell::new("Metric").add_attribute(Attribute::Bold),
        Cell::new("Custom").add_attribute(Attribute::Bold),
    ];
    if baseline.is_some() {
        header.push(Cell::new("Tokio (Base)").add_attribute(Attribute::Bold));
        header.push(Cell::new("Diff (%)").add_attribute(Attribute::Bold));
    }
    table.set_header(header);

    fn format_diff(v: f64, b: f64, higher_is_better: bool) -> Cell {
        let diff = if b == 0.0 { 0.0 } else { ((v - b) / b) * 100.0 };
        let text = format!("{:+.1}%", diff);
        if (diff > 0.0 && higher_is_better) || (diff < 0.0 && !higher_is_better) {
            Cell::new(text).fg(Color::Green)
        } else if diff == 0.0 {
            Cell::new(text)
        } else {
            Cell::new(text).fg(Color::Red)
        }
    }

    let mut row_throughput = vec![Cell::new("Throughput"), Cell::new(format!("{:.2} MiB/s", results.throughput))];
    if let Some(b) = baseline {
        row_throughput.push(Cell::new(format!("{:.2} MiB/s", b.throughput)));
        row_throughput.push(format_diff(results.throughput, b.throughput, true));
    }
    table.add_row(row_throughput);

    let mut row_mrate = vec![Cell::new("Message Rate"), Cell::new(format!("{:.0} msg/s", results.msg_rate))];
    if let Some(b) = baseline {
        row_mrate.push(Cell::new(format!("{:.0} msg/s", b.msg_rate)));
        row_mrate.push(format_diff(results.msg_rate, b.msg_rate, true));
    }
    table.add_row(row_mrate);

    let mut row_avg = vec![Cell::new("Avg Latency"), Cell::new(format!("{:.3} ms", results.avg))];
    if let Some(b) = baseline {
        row_avg.push(Cell::new(format!("{:.3} ms", b.avg)));
        row_avg.push(format_diff(results.avg, b.avg, false));
    }
    table.add_row(row_avg);

    let mut row_p50 = vec![Cell::new("P50 Latency"), Cell::new(format!("{} µs", results.p50))];
    if let Some(b) = baseline {
        row_p50.push(Cell::new(format!("{} µs", b.p50)));
        row_p50.push(format_diff(results.p50 as f64, b.p50 as f64, false));
    }
    table.add_row(row_p50);

    let mut row_p95 = vec![Cell::new("P95 Latency"), Cell::new(format!("{} µs", results.p95))];
    if let Some(b) = baseline {
        row_p95.push(Cell::new(format!("{} µs", b.p95)));
        row_p95.push(format_diff(results.p95 as f64, b.p95 as f64, false));
    }
    table.add_row(row_p95);

    let mut row_p99 = vec![Cell::new("P99 Latency"), Cell::new(format!("{} µs", results.p99))];
    if let Some(b) = baseline {
        row_p99.push(Cell::new(format!("{} µs", b.p99)));
        row_p99.push(format_diff(results.p99 as f64, b.p99 as f64, false));
    }
    table.add_row(row_p99);

    let mut row_max = vec![Cell::new("Max Latency"), Cell::new(format!("{} µs", results.max))];
    if let Some(b) = baseline {
        row_max.push(Cell::new(format!("{} µs", b.max)));
        row_max.push(format_diff(results.max as f64, b.max as f64, false));
    }
    table.add_row(row_max);

    println!("{}", table);
}

const BASELINE_FILE: &str = "tokio_baseline.csv";

fn load_baselines() -> HashMap<usize, BenchResults> {
    let mut baselines = HashMap::new();
    let file = match OpenOptions::new().read(true).open(BASELINE_FILE) {
        Ok(f) => f,
        Err(_) => return baselines,
    };

    let reader = BufReader::new(file);
    for line in reader.lines().skip(1) { // Skip header
        if let Ok(l) = line {
            let parts: Vec<&str> = l.split(',').collect();
            if parts.len() >= 8 {
                let concurrency = parts[0].trim().parse().unwrap_or(0);
                let res = BenchResults {
                    duration: Duration::ZERO,
                    throughput: parts[1].trim().parse().unwrap_or(0.0),
                    msg_rate: parts[2].trim().parse().unwrap_or(0.0),
                    avg: parts[3].trim().parse().unwrap_or(0.0),
                    p50: parts[4].trim().parse().unwrap_or(0),
                    p95: parts[5].trim().parse().unwrap_or(0),
                    p99: parts[6].trim().parse().unwrap_or(0),
                    max: parts[7].trim().parse().unwrap_or(0),
                };
                baselines.insert(concurrency, res);
            }
        }
    }
    baselines
}

fn save_baseline(concurrency: usize, results: BenchResults) {
    let mut baselines = load_baselines();
    baselines.insert(concurrency, results);
    
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(BASELINE_FILE)
        .unwrap();
    
    writeln!(file, "concurrency,throughput_mib_s,msg_rate,avg_ms,p50_us,p95_us,p99_us,max_us").unwrap();
    
    let mut sorted_keys: Vec<_> = baselines.keys().collect();
    sorted_keys.sort();
    
    for &k in &sorted_keys {
        let r = baselines.get(k).unwrap();
        writeln!(file, "{},{},{},{},{},{},{},{}", 
            k, r.throughput, r.msg_rate, r.avg, r.p50, r.p95, r.p99, r.max).unwrap();
    }
}

fn log_results(runtime: &str, concurrency: usize, results: BenchResults) {
    let file_exists = std::path::Path::new("results.csv").exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("results.csv")
        .unwrap();
    
    if !file_exists {
        writeln!(file, "runtime,concurrency,throughput_mib_s,msg_rate,avg_ms,p50_us,p95_us,p99_us,max_us").unwrap();
    }
    
    writeln!(file, "{},{},{},{},{},{},{},{},{}", 
        runtime, concurrency, results.throughput, results.msg_rate, results.avg, 
        results.p50, results.p95, results.p99, results.max).unwrap();
}

fn custom_runtime_echo(c: &mut Criterion) {
    let mut group = c.benchmark_group("TCP Echo Custom");
    group.sample_size(10);

    for &concurrency in CONCURRENCIES.iter() {
        let results_store = Arc::new(Mutex::new(Vec::new()));
        let results_store_clone = results_store.clone();
        
        let baselines = load_baselines();
        
        group.bench_with_input(BenchmarkId::from_parameter(concurrency), &concurrency, |b, &n| {
            let rt = runtime::Runtime::new();
            let messages_per_client = TOTAL_MESSAGES / n;
            let results_store_iter = results_store_clone.clone();

            b.iter_custom(|iters| {
                let results_store_iter = results_store_iter.clone();
                rt.block_on(async move {
                    let mut total_hist = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();
                    let start = Instant::now();
                    
                    for _it in 0..iters {
                        let listener = runtime::AsyncTcpListener::bind("127.0.0.1:0").unwrap();
                        let addr = listener.local_addr().unwrap();

                        // Server
                        let payload_size = PAYLOAD_SIZE;
                        runtime::spawn(async move {
                            for _ in 0..n {
                                if let Ok((stream, _)) = listener.accept().await {
                                    set_linger_zero(&stream);
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
                            }
                        });

                        // Clients
                        let mut handlers = Vec::new();
                        for _ in 0..n {
                            let addr = addr.clone();
                            let (tx, rx) = futures::channel::oneshot::channel();
                            runtime::spawn(async move {
                                let mut hist = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();
                                let mut stream = None;
                                for _ in 0..100 {
                                    if let Ok(s) = runtime::AsyncTcpStream::connect(&addr).await {
                                        set_linger_zero(&s);
                                        stream = Some(s);
                                        break;
                                    }
                                    runtime::sleep(Duration::from_millis(10)).await;
                                }

                                if let Some(stream) = stream {
                                    let payload = vec![0u8; payload_size];
                                    let mut buf = vec![0u8; payload_size];
                                    for _i in 0..messages_per_client {
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

                        for rx in handlers {
                            if let Ok(h) = rx.await {
                                total_hist.add(h).unwrap();
                            }
                        }
                    }
                    
                    let total_duration = start.elapsed();
                    let total_messages_iters = iters as usize * n * messages_per_client;
                    let throughput = (total_messages_iters * PAYLOAD_SIZE) as f64 / total_duration.as_secs_f64();
                    
                    let res = BenchResults {
                        duration: total_duration,
                        throughput: throughput / (1024.0 * 1024.0),
                        msg_rate: total_messages_iters as f64 / total_duration.as_secs_f64(),
                        avg: total_hist.mean() / 1_000_000.0,
                        p50: total_hist.value_at_quantile(0.5) / 1000,
                        p95: total_hist.value_at_quantile(0.95) / 1000,
                        p99: total_hist.value_at_quantile(0.99) / 1000,
                        max: total_hist.max() / 1000,
                    };
                    results_store_iter.lock().unwrap().push(res);
                    
                    total_duration
                })
            });
        });

        let final_results = results_store.lock().unwrap().clone();
        if !final_results.is_empty() {
            let aggregated = aggregate_median(final_results);
            let baseline = baselines.get(&concurrency).cloned();
            print_results_table("Custom Runtime", concurrency, TOTAL_MESSAGES as u64, aggregated, baseline);
            log_results("custom", concurrency, aggregated);
        }
    }
    group.finish();
}

fn tokio_runtime_echo(c: &mut Criterion) {
    let mut group = c.benchmark_group("TCP Echo Tokio");
    group.sample_size(10);
    
    let run_baseline = std::env::var("BASELINE").is_ok();
    let has_baseline_file = std::path::Path::new(BASELINE_FILE).exists();
    
    if !run_baseline && has_baseline_file {
        println!("\n\x1b[1;34mℹ️ Skipping Tokio benchmark. Using saved results from {}\x1b[0m", BASELINE_FILE);
        return;
    }

    for &concurrency in CONCURRENCIES.iter() {
        let results_store = Arc::new(Mutex::new(Vec::new()));
        let results_store_clone = results_store.clone();

        group.bench_with_input(BenchmarkId::from_parameter(concurrency), &concurrency, |b, &n| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            
            let messages_per_client = TOTAL_MESSAGES / n;
            let results_store_iter = results_store_clone.clone();

            b.iter_custom(|iters| {
                let results_store_iter = results_store_iter.clone();
                rt.block_on(async move {
                    let mut total_hist = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();
                    let start = Instant::now();
                    
                    for _ in 0..iters {
                        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                        let addr = listener.local_addr().unwrap();

                        // Server
                        let payload_size = PAYLOAD_SIZE;
                        tokio::spawn(async move {
                            for _ in 0..n {
                                if let Ok((mut stream, _)) = listener.accept().await {
                                    set_linger_zero(&stream);
                                    tokio::spawn(async move {
                                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
                            }
                        });

                        // Clients
                        let mut handles = Vec::new();
                        for _ in 0..n {
                            let (tx, rx) = futures::channel::oneshot::channel();
                            tokio::spawn(async move {
                                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                                let mut hist = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();
                                let mut stream = None;
                                for _ in 0..100 {
                                    if let Ok(s) = tokio::net::TcpStream::connect(addr).await {
                                        set_linger_zero(&s);
                                        stream = Some(s);
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                }

                                if let Some(mut stream) = stream {
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
                            handles.push(rx);
                        }

                        for rx in handles {
                            if let Ok(h) = rx.await {
                                total_hist.add(h).unwrap();
                            }
                        }
                    }
                    
                    let total_duration = start.elapsed();
                    let total_messages_iters = iters as usize * n * messages_per_client;
                    let throughput = (total_messages_iters * PAYLOAD_SIZE) as f64 / total_duration.as_secs_f64();
                    
                    let res = BenchResults {
                        duration: total_duration,
                        throughput: throughput / (1024.0 * 1024.0),
                        msg_rate: total_messages_iters as f64 / total_duration.as_secs_f64(),
                        avg: total_hist.mean() / 1_000_000.0,
                        p50: total_hist.value_at_quantile(0.5) / 1000,
                        p95: total_hist.value_at_quantile(0.95) / 1000,
                        p99: total_hist.value_at_quantile(0.99) / 1000,
                        max: total_hist.max() / 1000,
                    };
                    results_store_iter.lock().unwrap().push(res);
                    
                    total_duration
                })
            });
        });

        let final_results = results_store.lock().unwrap().clone();
        if !final_results.is_empty() {
            let aggregated = aggregate_median(final_results);
            print_results_table("Tokio Runtime", concurrency, TOTAL_MESSAGES as u64, aggregated, None);
            save_baseline(concurrency, aggregated);
            log_results("tokio", concurrency, aggregated);
        }
    }
    group.finish();
}

criterion_group!(benches, custom_runtime_echo, tokio_runtime_echo);
criterion_main!(benches);
