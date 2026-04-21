use anyhow::Result;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

/// One entry in the backend pool, with a live count of in-flight connections.
/// The in-flight counter drives least-connections routing.
#[derive(Clone)]
pub struct BackendEntry {
    pub url: String,
    pub in_flight: Arc<AtomicUsize>,
}

/// Shared backend pool — updated by the executor whenever workers are added/removed.
pub type SharedBackends = Arc<Mutex<Vec<BackendEntry>>>;

/// Maximum concurrent proxy forwarding threads.
/// Returns 503 as explicit backpressure rather than exhausting file descriptors.
const MAX_PROXY_CONCURRENCY: usize = 512;

struct Semaphore(Mutex<usize>, Condvar);

impl Semaphore {
    fn new(max: usize) -> Arc<Self> {
        Arc::new(Semaphore(Mutex::new(max), Condvar::new()))
    }

    fn try_acquire(self: &Arc<Self>) -> bool {
        let mut n = self.0.lock().unwrap();
        if *n > 0 { *n -= 1; true } else { false }
    }

    fn release(self: &Arc<Self>) {
        *self.0.lock().unwrap() += 1;
        self.1.notify_one();
    }
}

/// RAII guard that decrements an in-flight counter when dropped.
struct InFlightGuard(Arc<AtomicUsize>);
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Start a round-robin TCP proxy on `port` in a background thread.
/// Uses least-connections routing: each request goes to the backend with the
/// fewest in-flight connections, preventing overloaded workers from getting
/// more traffic while they drain their queue.
pub fn start_proxy(port: u16, backends: SharedBackends) -> Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    println!("[proxy] stable endpoint on {}  (send loadgen here)", addr);

    thread::spawn(move || {
        let sem = Semaphore::new(MAX_PROXY_CONCURRENCY);

        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    if !sem.try_acquire() {
                        // Explicit backpressure — no log spam.
                        let _ = stream.write(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 17\r\nConnection: close\r\n\r\nproxy at capacity\n",
                        );
                        continue;
                    }
                    let backends = backends.clone();
                    let sem      = sem.clone();
                    thread::spawn(move || {
                        forward(stream, backends);
                        sem.release();
                    });
                }
                Err(e) => {
                    eprintln!("[proxy] accept error: {}", e);
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    });

    Ok(())
}

fn forward(client: TcpStream, backends: SharedBackends) {
    let timeout = Duration::from_secs(60);
    let _ = client.set_read_timeout(Some(timeout));
    let _ = client.set_write_timeout(Some(timeout));
    let mut client = client;

    let mut buf = [0u8; 8192];
    let n = match client.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };

    // Least-connections: pick the backend with the fewest in-flight requests.
    // Increment its counter before releasing the lock; the guard decrements on drop.
    let (backend_addr, _guard) = {
        let mut locked = backends.lock().unwrap();
        if locked.is_empty() {
            let _ = client.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 16\r\nConnection: close\r\n\r\nno backends yet\n",
            );
            return;
        }
        let idx = locked
            .iter()
            .enumerate()
            .min_by_key(|(_, b)| b.in_flight.load(Ordering::Relaxed))
            .map(|(i, _)| i)
            .unwrap_or(0);
        locked[idx].in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = InFlightGuard(locked[idx].in_flight.clone());
        let addr = locked[idx].url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("127.0.0.1:9000")
            .to_string();
        (addr, guard)
    };

    let mut backend = match TcpStream::connect(&backend_addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[proxy] connect to {} failed: {}", backend_addr, e);
            let _ = client.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 12\r\nConnection: close\r\n\r\nbad gateway\n",
            );
            return;
        }
    };
    let _ = backend.set_read_timeout(Some(timeout));
    let _ = backend.set_write_timeout(Some(timeout));

    if backend.write_all(&buf[..n]).is_err() { return; }
    if backend.flush().is_err() { return; }

    let mut response = Vec::new();
    let _ = backend.read_to_end(&mut response);
    let _ = client.write_all(&response);
    // _guard drops here, decrementing in_flight
}
