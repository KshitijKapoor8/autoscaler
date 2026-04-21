use std::collections::VecDeque;
use std::hint::black_box;
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
    pub worker_threads: usize,
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

/// Fixed-size thread pool for CPU-bound work.
///
/// Requests submit a closure and block until it completes.  With N threads and
/// more than N concurrent requests, callers queue inside the pool — giving real
/// CPU backpressure without spawning unbounded OS threads.
struct CpuThreadPool {
    sender: std::sync::mpsc::Sender<Box<dyn FnOnce() + Send + 'static>>,
}

impl CpuThreadPool {
    fn new(num_threads: usize) -> Arc<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<Box<dyn FnOnce() + Send + 'static>>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..num_threads.max(1) {
            let rx = rx.clone();
            thread::spawn(move || loop {
                match rx.lock().unwrap().recv() {
                    Ok(job) => job(),
                    Err(_)  => break,
                }
            });
        }
        Arc::new(Self { sender: tx })
    }

    /// Submit a job and block the calling thread until it finishes.
    fn run<F: FnOnce() + Send + 'static>(&self, f: F) {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        self.sender
            .send(Box::new(move || { f(); let _ = done_tx.send(()); }))
            .expect("cpu threadpool workers have stopped");
        done_rx.recv().expect("cpu threadpool job panicked");
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

pub fn run_service(port: u16, cpu_factor: f64, mem_factor: f64, max_concurrency: usize, worker_threads: usize) -> anyhow::Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    println!(
        "[service:{}] listening (cpu_factor={:.2}, mem_factor={:.2}, max_concurrency={}, worker_threads={})",
        port, cpu_factor, mem_factor, max_concurrency, worker_threads
    );

    let metrics = ServiceMetrics::default();
    let config = ServiceConfig { cpu_factor, mem_factor, max_concurrency, worker_threads };
    let semaphore = Semaphore::new(max_concurrency);
    let pool = CpuThreadPool::new(worker_threads);

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
                let pool = pool.clone();
                metrics.in_flight.fetch_add(1, Ordering::Relaxed);
                thread::spawn(move || handle_client(stream, metrics, config, semaphore, pool));
            }
            Err(e) => eprintln!("Failed to accept connection: {}", e),
        }
    }

    Ok(())
}

fn handle_client(mut stream: TcpStream, metrics: ServiceMetrics, config: ServiceConfig, semaphore: Semaphore, pool: Arc<CpuThreadPool>) {
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
        let iters = (2_000_000.0 * config.cpu_factor).round() as u64;
        pool.run(move || do_cpu_work(iters));
        write_response(&mut stream, 200, "OK", "cpu-heavy done");
        true
    } else if path.starts_with("/mem-heavy") {
        metrics.mem_requests.fetch_add(1, Ordering::Relaxed);
        let bytes = (10 * 1024 * 1024) as f64 * config.mem_factor;
        let _held = hold_memory(bytes as usize); // stays alive until end of this block
        thread::sleep(Duration::from_millis(50));
        write_response(&mut stream, 200, "OK", "mem-heavy done");
        true
    } else if path.starts_with("/mixed") {
        metrics.mixed_requests.fetch_add(1, Ordering::Relaxed);
        let iters = (1_000_000.0 * config.cpu_factor).round() as u64;
        let bytes = (5 * 1024 * 1024) as f64 * config.mem_factor;
        let _held = hold_memory(bytes as usize);
        pool.run(move || do_cpu_work(iters));
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
            "requests_total={}\nin_flight={}\nerrors_total={}\navg_latency_ms={:.2}\nmax_concurrency={}\nactive_count={}\nqueue_depth={}\ncpu_requests={}\nmem_requests={}\nmixed_requests={}\nos_cpu_ticks={}\nrss_kb={}\n",
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
            read_proc_cpu_ticks(),
            read_proc_rss_kb(),
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

/// Read cumulative CPU ticks (utime + stime) for this process from /proc/self/stat.
/// Returns 0 on any error or non-Linux platform.
fn read_proc_cpu_ticks() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let Ok(data) = std::fs::read_to_string("/proc/self/stat") else { return 0 };
        // /proc/self/stat: "pid (comm) state ppid ..."
        // comm can contain spaces/parens, so find the LAST ')' to skip it.
        let after_comm = match data.rfind(')') {
            Some(i) => &data[i + 1..],
            None => return 0,
        };
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        // After ')': field 3 = index 0, field 14 (utime) = index 11, field 15 (stime) = index 12
        let utime = fields.get(11).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let stime = fields.get(12).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        utime + stime
    }
    #[cfg(not(target_os = "linux"))]
    { 0 }
}

/// Read resident set size in KB for this process from /proc/self/status.
fn read_proc_rss_kb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let Ok(data) = std::fs::read_to_string("/proc/self/status") else { return 0 };
        for line in data.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                return rest.split_whitespace().next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            }
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    { 0 }
}

/// Real CPU-bound work using non-trivial integer mixing that the compiler cannot eliminate.
///
/// Throughput is roughly 200–500M iterations/sec per thread depending on hardware.
/// Default calibration at that rate:
///   cpu_factor=1.0 → 2M iters → ~5–10ms  → 1 thread saturates at ~100–200 RPS
///   cpu_factor=2.0 → 4M iters → ~10–20ms → 1 thread saturates at ~50–100 RPS
///
/// If the latency is too low or too high for your hardware, tune the constant (2_000_000).
fn do_cpu_work(iters: u64) {
    let mut state: u64 = 0x517c_c1b7_2722_0a95;
    for i in 0..iters {
        state ^= black_box(i).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        state  = black_box(state).rotate_left(31).wrapping_add(0x6c62_272e_07bb_0142);
    }
    black_box(state);
}

/// Allocate `bytes` of memory, touch every page to ensure it is resident in RAM,
/// and return the Vec so the caller can hold it alive for the full request duration.
/// Dropping the returned Vec releases the memory — simulating realistic RSS pressure
/// under concurrent load.
fn hold_memory(bytes: usize) -> Vec<u8> {
    let mut v = vec![0u8; bytes];
    for i in (0..bytes).step_by(4096) {
        v[i] = black_box(1u8);
    }
    v
}
