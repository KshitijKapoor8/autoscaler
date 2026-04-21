use crate::http::proxy::{BackendEntry, SharedBackends};
use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

pub struct Worker {
    pub port: u16,
    child: Child,
}

/// Manages per-worker cgroup sub-directories for OS-level CPU throttling.
///
/// Requires the controller to be running inside a properly delegated cgroup
/// (use `systemd-run --user --scope -- cargo run -- http-control …`).
/// If delegation is not available, `try_init()` returns None and the executor
/// falls back to the simulated concurrency-based approach.
struct CgroupManager {
    /// Path to our own cgroup directory, e.g.
    /// /sys/fs/cgroup/user.slice/…/autoscaler-ctl.scope
    dir: PathBuf,
}

impl CgroupManager {
    fn try_init() -> Option<Self> {
        let text = fs::read_to_string("/proc/self/cgroup").ok()?;
        let rel = text.lines()
            .find(|l| l.starts_with("0::"))?
            .strip_prefix("0::")?
            .trim()
            .to_string();

        let dir = PathBuf::from(format!("/sys/fs/cgroup{}", rel));

        // Enable cpu controller for children — fails if the cgroup isn't delegated to us.
        if fs::write(dir.join("cgroup.subtree_control"), "+cpu").is_err() {
            return None;
        }

        println!("[executor] cgroup mode active: {}", dir.display());
        Some(Self { dir })
    }

    fn worker_dir(&self, port: u16) -> PathBuf {
        self.dir.join(format!("worker-{}", port))
    }

    /// Create sub-cgroup and set initial CPU quota.
    fn setup(&self, port: u16, cpu_per_replica: f64) -> Result<()> {
        fs::create_dir_all(self.worker_dir(port))?;
        self.set_cpu_max(port, cpu_per_replica)
    }

    /// Update cpu.max in-place — the kernel applies the new quota immediately,
    /// no process restart required.  This is the vertical scaling action.
    fn set_cpu_max(&self, port: u16, cpu_per_replica: f64) -> Result<()> {
        // period = 100ms; quota = cpu_per_replica × 100ms
        let quota_us = (cpu_per_replica * 100_000.0).round().max(10_000.0) as u64;
        fs::write(
            self.worker_dir(port).join("cpu.max"),
            format!("{} 100000", quota_us),
        )?;
        Ok(())
    }

    /// Move the worker process into its own sub-cgroup.
    fn assign_pid(&self, port: u16, pid: u32) -> Result<()> {
        fs::write(self.worker_dir(port).join("cgroup.procs"), pid.to_string())?;
        Ok(())
    }

    /// Remove the sub-cgroup.  Must be called after the process has exited.
    fn teardown(&self, port: u16) {
        let _ = fs::remove_dir(self.worker_dir(port));
    }
}

pub struct Executor {
    workers: Vec<Worker>,
    base_port: u16,
    /// Fixed per experiment — workload type (cpu/mem intensity per request).
    cpu_factor: f64,
    mem_factor: f64,
    /// Current vertical-scale value (cpu_per_replica from controller).
    cpu_per_replica: f64,
    /// Concurrency semaphore slots per worker.
    /// In cgroup mode this is fixed high (safety valve only); the real speed
    /// limit is the OS cpu.max quota.  In fallback mode this is the vertical knob.
    max_concurrency: usize,
    /// Shared with the proxy so it always has the current backend list.
    pub backends: SharedBackends,
    cgroup: Option<CgroupManager>,
}

impl Executor {
    pub fn new(base_port: u16, cpu_factor: f64, mem_factor: f64, initial_cpu_per_replica: f64) -> Self {
        let cgroup = CgroupManager::try_init();
        // In cgroup mode the semaphore is just a safety valve against runaway
        // memory; the real throttle is cpu.max.  In fallback mode it IS the
        // vertical knob (cpu_per_replica × 4 slots).
        let max_concurrency = if cgroup.is_some() {
            32
        } else {
            eprintln!("[executor] cgroup delegation unavailable — using simulated (concurrency) vertical scaling");
            eprintln!("[executor] tip: run via  systemd-run --user --scope -- cargo run -- http-control ...");
            cpu_to_concurrency(initial_cpu_per_replica)
        };

        Self {
            workers: Vec::new(),
            base_port,
            cpu_factor,
            mem_factor,
            cpu_per_replica: initial_cpu_per_replica,
            max_concurrency,
            backends: Arc::new(Mutex::new(Vec::new())),
            cgroup,
        }
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub fn worker_urls(&self) -> Vec<String> {
        self.workers
            .iter()
            .map(|w| format!("http://127.0.0.1:{}", w.port))
            .collect()
    }

    /// Returns (port, cpu_per_replica) for each running worker.
    pub fn worker_states(&self) -> Vec<(u16, f64)> {
        self.workers.iter().map(|w| (w.port, self.cpu_per_replica)).collect()
    }

    /// Apply a new target from the controller.
    ///
    /// Vertical scaling behaviour:
    ///   - cgroup mode: updates cpu.max in-place on every running worker —
    ///     no restart.  The OS throttles immediately.
    ///   - fallback mode: restarts workers with a new concurrency slot count.
    pub fn apply_target(&mut self, target_replicas: u32, cpu_per_replica: f64, _mem_per_replica: f64) -> Result<()> {
        let cpu_changed = (cpu_per_replica - self.cpu_per_replica).abs() > 0.01;
        self.cpu_per_replica = cpu_per_replica;

        if cpu_changed {
            if self.cgroup.is_some() {
                // --- OS-level vertical scale: just write new quota, no restart ---
                for worker in &self.workers {
                    if let Some(ref cg) = self.cgroup {
                        match cg.set_cpu_max(worker.port, cpu_per_replica) {
                            Ok(()) => println!(
                                "[executor] :{} cpu.max → {:.2} CPUs ({} µs / 100ms period)",
                                worker.port, cpu_per_replica,
                                (cpu_per_replica * 100_000.0).round() as u64
                            ),
                            Err(e) => eprintln!("[executor] cgroup update :{}: {}", worker.port, e),
                        }
                    }
                }
            } else {
                // --- Fallback: restart workers with new concurrency slot count ---
                let new_concurrency = cpu_to_concurrency(cpu_per_replica);
                while let Some(mut w) = self.workers.pop() {
                    println!("[executor] restarting :{} with max_concurrency={}", w.port, new_concurrency);
                    let _ = w.child.kill();
                    let _ = w.child.wait();
                }
                self.max_concurrency = new_concurrency;
                self.sync_backends();
            }
        }

        self.ensure_workers(target_replicas as usize)
    }

    /// Ensure exactly `target` workers are running (horizontal scale up/down).
    pub fn ensure_workers(&mut self, target: usize) -> Result<()> {
        // worker_threads = how many CPU threads each worker process gets.
        // Capped at max_concurrency so we never have more CPU threads than semaphore slots.
        let worker_threads = (self.cpu_per_replica.ceil() as usize).max(1).min(self.max_concurrency);
        while self.workers.len() < target {
            let port = self.base_port + self.workers.len() as u16;
            let worker = spawn_worker(
                port, self.cpu_factor, self.mem_factor,
                self.max_concurrency, self.cpu_per_replica,
                worker_threads,
                self.cgroup.as_ref(),
            )?;
            if self.cgroup.is_some() {
                println!("[executor] started worker :{} (cpu_quota={:.2} CPUs, max_concurrency={}, worker_threads={})",
                    port, self.cpu_per_replica, self.max_concurrency, worker_threads);
            } else {
                println!("[executor] started worker :{} (max_concurrency={}, worker_threads={})", port, self.max_concurrency, worker_threads);
            }
            self.workers.push(worker);
            self.sync_backends();
        }

        while self.workers.len() > target {
            if let Some(mut w) = self.workers.pop() {
                println!("[executor] stopped worker :{}", w.port);
                let _ = w.child.kill();
                let _ = w.child.wait();
                if let Some(ref cg) = self.cgroup {
                    cg.teardown(w.port);
                }
                self.sync_backends();
            }
        }

        Ok(())
    }

    fn sync_backends(&self) {
        let mut locked = self.backends.lock().unwrap();
        let new_urls: Vec<String> = self.workers
            .iter()
            .map(|w| format!("http://127.0.0.1:{}", w.port))
            .collect();

        // Remove entries for workers that no longer exist.
        locked.retain(|e| new_urls.contains(&e.url));

        // Add entries for newly started workers (in_flight starts at 0).
        for url in &new_urls {
            if !locked.iter().any(|e| &e.url == url) {
                locked.push(BackendEntry {
                    url: url.clone(),
                    in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                });
            }
        }

        // Keep the list in the same order as self.workers.
        locked.sort_by_key(|e| new_urls.iter().position(|u| u == &e.url).unwrap_or(usize::MAX));
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        while let Some(mut w) = self.workers.pop() {
            let _ = w.child.kill();
            let _ = w.child.wait();
            if let Some(ref cg) = self.cgroup {
                cg.teardown(w.port);
            }
            println!("[executor] killed worker :{} (shutdown)", w.port);
        }
    }
}

/// Map the controller's abstract cpu_per_replica (0.5–4.0) to a semaphore slot count.
/// Used only in fallback (no-cgroup) mode.
fn cpu_to_concurrency(cpu_per_replica: f64) -> usize {
    (cpu_per_replica * 4.0).round().max(1.0) as usize
}

fn spawn_worker(
    port: u16,
    cpu_factor: f64,
    mem_factor: f64,
    max_concurrency: usize,
    cpu_per_replica: f64,
    worker_threads: usize,
    cgroup: Option<&CgroupManager>,
) -> Result<Worker> {
    // Create and configure the sub-cgroup before spawning so the quota is
    // in place before the process starts doing work.
    if let Some(cg) = cgroup {
        cg.setup(port, cpu_per_replica)?;
    }

    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .arg("service")
        .arg("--port").arg(port.to_string())
        .arg("--cpu-factor").arg(format!("{:.3}", cpu_factor))
        .arg("--mem-factor").arg(format!("{:.3}", mem_factor))
        .arg("--max-concurrency").arg(max_concurrency.to_string())
        .arg("--worker-threads").arg(worker_threads.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    // Move the new PID into its dedicated sub-cgroup.
    if let Some(cg) = cgroup {
        if let Err(e) = cg.assign_pid(port, child.id()) {
            eprintln!("[executor] failed to assign PID {} to worker cgroup :{}: {}", child.id(), port, e);
        }
    }

    Ok(Worker { port, child })
}

