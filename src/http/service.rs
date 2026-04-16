use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, atomic::{AtomicU64, AtomicUsize, Ordering}};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub struct ServiceConfig {
    pub cpu_factor: f64,
    pub mem_factor: f64,
    pub max_concurrency: usize,
}

/// Counting semaphore: limits how many requests are actively processed at once.
/// Requests that arrive when the semaphore is full wait here — this is the queue.
#[derive(Clone)]
struct Semaphore {
    inner: Arc<(Mutex<usize>, Condvar)>,
    capacity: usize,
}

impl Semaphore {
    fn new(capacity: usize) -> Self {
        Self { inner: Arc::new((Mutex::new(0), Condvar::new())), capacity }
    }

    fn acquire(&self) -> SemaphoreGuard {
        let (lock, cvar) = &*self.inner;
        let mut active = lock.lock().unwrap();
        while *active >= self.capacity {
            active = cvar.wait(active).unwrap();
        }
        *active += 1;
        SemaphoreGuard { inner: self.inner.clone() }
    }

    fn active(&self) -> usize {
        *self.inner.0.lock().unwrap()
    }
}

struct SemaphoreGuard {
    inner: Arc<(Mutex<usize>, Condvar)>,
}

impl Drop for SemaphoreGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.inner;
        *lock.lock().unwrap() -= 1;
        cvar.notify_one();
    }
}

#[derive(Clone, Default)]
pub struct ServiceMetrics {
    pub total_requests: Arc<AtomicU64>,
    pub in_flight: Arc<AtomicUsize>,
    pub active_count: Arc<AtomicUsize>,  // past the semaphore, doing real work
    pub queue_depth: Arc<AtomicUsize>,   // waiting for the semaphore
    pub total_errors: Arc<AtomicU64>,
    // Rolling window: last 100 completed request latencies
    recent_latencies: Arc<Mutex<VecDeque<u64>>>,
    pub cpu_requests: Arc<AtomicU64>,
    pub mem_requests: Arc<AtomicU64>,
    pub mixed_requests: Arc<AtomicU64>,
}

const LATENCY_WINDOW: usize = 100;

impl ServiceMetrics {
    fn record_request(&self, latency_ms: u64, success: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if !success {
            self.total_errors.fetch_add(1, Ordering::Relaxed);
        } else {
            let mut w = self.recent_latencies.lock().unwrap();
            if w.len() >= LATENCY_WINDOW {
                w.pop_front();
            }
            w.push_back(latency_ms);
        }
    }

    fn avg_latency_ms(&self) -> f64 {
        let w = self.recent_latencies.lock().unwrap();
        if w.is_empty() { return 0.0; }
        w.iter().sum::<u64>() as f64 / w.len() as f64
    }
}

pub fn run_service(port: u16, cpu_factor: f64, mem_factor: f64, max_concurrency: usize) -> anyhow::Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    println!(
        "[service:{}] listening (cpu_factor={:.2}, mem_factor={:.2}, max_concurrency={})",
        port, cpu_factor, mem_factor, max_concurrency
    );

    let metrics = ServiceMetrics::default();
    let config = ServiceConfig { cpu_factor, mem_factor, max_concurrency };
    let semaphore = Semaphore::new(max_concurrency);

    // Periodic summary logging
    {
        let m = metrics.clone();
        let sem = semaphore.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(5));
            println!(
                "[service:{}] total={} errors={} active={}/{} queued={} avg_latency_ms={:.2}",
                port,
                m.total_requests.load(Ordering::Relaxed),
                m.total_errors.load(Ordering::Relaxed),
                sem.active(),
                max_concurrency,
                m.queue_depth.load(Ordering::Relaxed),
                m.avg_latency_ms(),
            );
        });
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let metrics = metrics.clone();
                let semaphore = semaphore.clone();
                metrics.in_flight.fetch_add(1, Ordering::Relaxed);
                thread::spawn(move || handle_client(stream, metrics, config, semaphore));
            }
            Err(e) => eprintln!("Failed to accept connection: {}", e),
        }
    }

    Ok(())
}

fn handle_client(mut stream: TcpStream, metrics: ServiceMetrics, config: ServiceConfig, semaphore: Semaphore) {
    let start = Instant::now();

    let mut buf = [0u8; 1024];
    let mut request = Vec::new();

    match stream.read(&mut buf) {
        Ok(0) | Err(_) => { metrics.in_flight.fetch_sub(1, Ordering::Relaxed); return; }
        Ok(n) => request.extend_from_slice(&buf[..n]),
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

    // Metrics and healthz bypass the concurrency semaphore so they always respond
    if method == "GET" && (path.starts_with("/metrics") || path.starts_with("/healthz")) {
        serve_meta(path, &mut stream, &metrics, config.max_concurrency, &semaphore);
        metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
        return;
    }

    if method != "GET" {
        write_response(&mut stream, 405, "Method Not Allowed", "Only GET supported");
        metrics.record_request(0, false);
        metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
        return;
    }

    // Acquire semaphore — excess requests queue here until a slot is free
    metrics.queue_depth.fetch_add(1, Ordering::Relaxed);
    let _guard = semaphore.acquire();
    metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
    metrics.active_count.fetch_add(1, Ordering::Relaxed);

    let success = if path.starts_with("/cpu-heavy") {
        metrics.cpu_requests.fetch_add(1, Ordering::Relaxed);
        simulate_cpu_work((10_000_000.0 * config.cpu_factor).max(1.0) as u64);
        write_response(&mut stream, 200, "OK", "cpu-heavy done");
        true
    } else if path.starts_with("/mem-heavy") {
        metrics.mem_requests.fetch_add(1, Ordering::Relaxed);
        simulate_memory_work((10.0 * 1024.0 * 1024.0 * config.mem_factor).max(1024.0) as usize);
        write_response(&mut stream, 200, "OK", "mem-heavy done");
        true
    } else if path.starts_with("/mixed") {
        metrics.mixed_requests.fetch_add(1, Ordering::Relaxed);
        simulate_cpu_work((5_000_000.0 * config.cpu_factor).max(1.0) as u64);
        simulate_memory_work((5.0 * 1024.0 * 1024.0 * config.mem_factor).max(1024.0) as usize);
        write_response(&mut stream, 200, "OK", "mixed done");
        true
    } else {
        write_response(&mut stream, 404, "Not Found", "Unknown path");
        false
    };

    metrics.active_count.fetch_sub(1, Ordering::Relaxed);
    metrics.record_request(start.elapsed().as_millis() as u64, success);
    metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
}

fn serve_meta(path: &str, stream: &mut TcpStream, metrics: &ServiceMetrics, max_concurrency: usize, semaphore: &Semaphore) {
    if path.starts_with("/metrics") {
        let total = metrics.total_requests.load(Ordering::Relaxed);
        let errors = metrics.total_errors.load(Ordering::Relaxed);
        let body = format!(
            "requests_total={}\nin_flight={}\nerrors_total={}\navg_latency_ms={:.2}\nmax_concurrency={}\nactive_count={}\nqueue_depth={}\ncpu_requests={}\nmem_requests={}\nmixed_requests={}\n",
            total,
            metrics.in_flight.load(Ordering::Relaxed),
            errors,
            metrics.avg_latency_ms(),
            max_concurrency,
            semaphore.active(),
            metrics.queue_depth.load(Ordering::Relaxed),
            metrics.cpu_requests.load(Ordering::Relaxed),
            metrics.mem_requests.load(Ordering::Relaxed),
            metrics.mixed_requests.load(Ordering::Relaxed),
        );
        write_response(stream, 200, "OK", &body);
    } else {
        write_response(stream, 200, "OK", "ok");
    }
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
