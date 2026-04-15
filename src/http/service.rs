use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, atomic::{AtomicU64, AtomicUsize, Ordering}};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub struct ServiceConfig {
    pub cpu_factor: f64,
    pub mem_factor: f64,
}

#[derive(Clone, Default)]
pub struct ServiceMetrics {
    pub total_requests: Arc<AtomicU64>,
    pub in_flight: Arc<AtomicUsize>,
    pub total_errors: Arc<AtomicU64>,
    pub total_latency_ms: Arc<AtomicU64>,
    pub completed_requests: Arc<AtomicU64>,
    pub cpu_requests: Arc<AtomicU64>,
    pub mem_requests: Arc<AtomicU64>,
    pub mixed_requests: Arc<AtomicU64>,
}
impl ServiceMetrics {
    fn record_request(&self, latency_ms: u64, success: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if !success {
            self.total_errors.fetch_add(1, Ordering::Relaxed);
        } else {
            self.completed_requests.fetch_add(1, Ordering::Relaxed);
            self.total_latency_ms
                .fetch_add(latency_ms, Ordering::Relaxed);
        }
    }

    fn avg_latency_ms(&self) -> f64 {
        let completed = self.completed_requests.load(Ordering::Relaxed);
        if completed == 0 {
            0.0
        } else {
            self.total_latency_ms.load(Ordering::Relaxed) as f64 / completed as f64
        }
    }
}

pub fn run_service(port: u16, cpu_factor: f64, mem_factor: f64) -> anyhow::Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    println!("Toy HTTP service listening on {} (cpu_factor={:.2}, mem_factor={:.2})", addr, cpu_factor, mem_factor);

    let metrics = ServiceMetrics::default();
    let config = ServiceConfig { cpu_factor, mem_factor };

    // Periodic summary logging
    {
        let metrics_clone = metrics.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(5));
            let total = metrics_clone.total_requests.load(Ordering::Relaxed);
            let errors = metrics_clone.total_errors.load(Ordering::Relaxed);
            let in_flight = metrics_clone.in_flight.load(Ordering::Relaxed);
            let cpu = metrics_clone.cpu_requests.load(Ordering::Relaxed);
            let mem = metrics_clone.mem_requests.load(Ordering::Relaxed);
            let mixed = metrics_clone.mixed_requests.load(Ordering::Relaxed);
            let avg_lat = metrics_clone.avg_latency_ms();
            println!(
                "[service] total={} errors={} in_flight={} cpu={} mem={} mixed={} avg_latency_ms={:.2}",
                total, errors, in_flight, cpu, mem, mixed, avg_lat
            );
        });
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let metrics = metrics.clone();
                let config = config;
                metrics.in_flight.fetch_add(1, Ordering::Relaxed);
                thread::spawn(move || {
                    handle_client(stream, metrics, config);
                });
            }
            Err(e) => {
                eprintln!("Failed to accept connection: {}", e);
            }
        }
    }

    Ok(())
}

fn handle_client(mut stream: TcpStream, metrics: ServiceMetrics, config: ServiceConfig) {
    let start = Instant::now();

    let mut buf = [0u8; 1024];
    let mut request = Vec::new();

    // Read until end of headers or buffer full
    match stream.read(&mut buf) {
        Ok(0) | Err(_) => {
            metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
            return;
        }
        Ok(n) => {
            request.extend_from_slice(&buf[..n]);
        }
    }

    let request_str = match std::str::from_utf8(&request) {
        Ok(s) => s,
        Err(_) => {
            write_response(&mut stream, 400, "Bad Request", "Invalid UTF-8");
            metrics.record_request(0, false);
            metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    };

    let (method, path) = match parse_request_line(request_str) {
        Some(v) => v,
        None => {
            write_response(&mut stream, 400, "Bad Request", "Malformed request line");
            metrics.record_request(0, false);
            metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    };

    let mut success = true;

    if method != "GET" {
        write_response(&mut stream, 405, "Method Not Allowed", "Only GET supported");
        success = false;
    } else if path.starts_with("/cpu-heavy") {
        metrics.cpu_requests.fetch_add(1, Ordering::Relaxed);
        let iters = (10_000_000.0 * config.cpu_factor).max(1.0) as u64;
        simulate_cpu_work(iters);
        write_response(&mut stream, 200, "OK", "cpu-heavy done");
    } else if path.starts_with("/mem-heavy") {
        metrics.mem_requests.fetch_add(1, Ordering::Relaxed);
        let bytes = (10.0 * 1024.0 * 1024.0 * config.mem_factor).max(1024.0) as usize;
        simulate_memory_work(bytes);
        write_response(&mut stream, 200, "OK", "mem-heavy done");
    } else if path.starts_with("/mixed") {
        metrics.mixed_requests.fetch_add(1, Ordering::Relaxed);
        let iters = (5_000_000.0 * config.cpu_factor).max(1.0) as u64;
        let bytes = (5.0 * 1024.0 * 1024.0 * config.mem_factor).max(1024.0) as usize;
        simulate_cpu_work(iters);
        simulate_memory_work(bytes);
        write_response(&mut stream, 200, "OK", "mixed done");
    } else if path.starts_with("/metrics") {
        let body = format!(
            "requests_total={}\nin_flight={}\nerrors_total={}\navg_latency_ms={:.2}\ncpu_requests={}\nmem_requests={}\nmixed_requests={}\n",
            metrics.total_requests.load(Ordering::Relaxed),
            metrics.in_flight.load(Ordering::Relaxed),
            metrics.total_errors.load(Ordering::Relaxed),
            metrics.avg_latency_ms(),
            metrics.cpu_requests.load(Ordering::Relaxed),
            metrics.mem_requests.load(Ordering::Relaxed),
            metrics.mixed_requests.load(Ordering::Relaxed),
        );
        write_response(&mut stream, 200, "OK", &body);
    } else if path.starts_with("/healthz") {
        write_response(&mut stream, 200, "OK", "ok");
    } else {
        write_response(&mut stream, 404, "Not Found", "Unknown path");
        success = false;
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;
    metrics.record_request(elapsed_ms, success);
    metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
}

fn parse_request_line(request: &str) -> Option<(&str, &str)> {
    let mut lines = request.lines();
    let line = lines.next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method, path))
}

fn write_response(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{}",
        status,
        reason,
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn simulate_cpu_work(iters: u64) {
    let mut x = 0u64;
    for i in 0..iters {
        x = x.wrapping_add(i.rotate_left(13) ^ 0xDEADBEEF);
        if i % 1_000_000 == 0 {
            std::hint::spin_loop();
        }
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    let _ = x;
}

fn simulate_memory_work(bytes: usize) {
    let mut v = Vec::with_capacity(bytes);
    v.resize(bytes, 0u8);
    // Touch memory to ensure it's actually used
    for i in (0..bytes).step_by(4096) {
        v[i] = 1;
    }
    // Keep it alive briefly
    thread::sleep(Duration::from_millis(10));
}
