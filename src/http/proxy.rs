use anyhow::Result;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Shared backend list — updated by the executor whenever workers are added or removed.
pub type SharedBackends = Arc<Mutex<Vec<String>>>;

/// Start a round-robin TCP proxy on `port` in a background thread.
/// Traffic is distributed across whatever URLs are in `backends` at request time.
pub fn start_proxy(port: u16, backends: SharedBackends) -> Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    println!("[proxy] stable endpoint on {}  (send loadgen here)", addr);

    thread::spawn(move || {
        let counter = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let backends = backends.clone();
                    let counter = counter.clone();
                    thread::spawn(move || forward(stream, backends, counter));
                }
                Err(e) => eprintln!("[proxy] accept error: {}", e),
            }
        }
    });

    Ok(())
}

fn forward(mut client: TcpStream, backends: SharedBackends, counter: Arc<AtomicUsize>) {
    let mut buf = [0u8; 4096];
    let n = match client.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };

    let backend_addr = {
        let backends = backends.lock().unwrap();
        if backends.is_empty() {
            let _ = client.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 16\r\nConnection: close\r\n\r\nno backends yet\n",
            );
            return;
        }
        let idx = counter.fetch_add(1, Ordering::Relaxed) % backends.len();
        // Strip "http://" prefix to get "host:port"
        backends[idx]
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("127.0.0.1:9000")
            .to_string()
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

    if backend.write_all(&buf[..n]).is_err() { return; }
    if backend.flush().is_err() { return; }

    let mut response = Vec::new();
    let _ = backend.read_to_end(&mut response);
    let _ = client.write_all(&response);
}
