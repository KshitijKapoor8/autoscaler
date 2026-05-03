use anyhow::{bail, Result};
use std::process::Command;
use std::sync::Arc;
use crate::http::proxy::{BackendEntry, SharedBackends};

const NETWORK_NAME: &str = "autoscaler-net";
const WORKER_PORT: u16 = 8080;

/// Identifies a running worker container.
pub struct DockerWorkerInfo {
    /// Container name, e.g. "autoscaler-worker-0".
    pub name: String,
    /// Full URL reachable from the host, e.g. "http://127.0.0.1:49234".
    pub url: String,
}

impl DockerWorkerInfo {
    pub fn url(&self) -> String {
        self.url.clone()
    }
}

/// Snapshot of a worker's identity and current vertical-scale config.
/// Returned to the controller loop for per-worker metrics fetching and CSV logging.
pub struct WorkerState {
    pub name: String,
    pub url: String,
    pub cpu_quota: f64,
}

/// Manages Docker worker containers.
///
/// Horizontal scaling → start/stop containers.
/// Vertical scaling   → `docker update --cpus` in-place (no restart, same semantics
///                       as cgroup cpu.max in the OS-process executor).
pub struct DockerExecutor {
    workers: Vec<DockerWorkerInfo>,
    image: String,
    cpu_factor: f64,
    mem_factor: f64,
    /// Current vertical-scale value (cpu_per_replica from controller).
    pub cpu_per_replica: f64,
    /// Shared with the proxy so it always has the current backend list.
    pub backends: SharedBackends,
}

impl DockerExecutor {
    /// Create the executor, ensure the Docker bridge network exists,
    /// and remove any stale containers from a previous run.
    pub fn new(
        image: &str,
        cpu_factor: f64,
        mem_factor: f64,
        initial_cpu_per_replica: f64,
    ) -> Result<Self> {
        ensure_network()?;
        cleanup_stale_workers();
        Ok(Self {
            workers: Vec::new(),
            image: image.to_string(),
            cpu_factor,
            mem_factor,
            cpu_per_replica: initial_cpu_per_replica,
            backends: Arc::new(std::sync::Mutex::new(Vec::new())),
        })
    }

    pub fn worker_states(&self) -> Vec<WorkerState> {
        self.workers
            .iter()
            .map(|w| WorkerState {
                name: w.name.clone(),
                url: w.url(),
                cpu_quota: self.cpu_per_replica,
            })
            .collect()
    }

    /// Apply a new target from the controller.
    ///
    /// Vertical scaling: `docker update --cpus <N>` — in-place, no container restart.
    /// Horizontal scaling: start or stop containers.
    pub fn apply_target(
        &mut self,
        target_replicas: u32,
        cpu_per_replica: f64,
        _mem_per_replica: f64,
    ) -> Result<()> {
        let cpu_changed = (cpu_per_replica - self.cpu_per_replica).abs() > 0.01;
        self.cpu_per_replica = cpu_per_replica;

        if cpu_changed {
            for w in &self.workers {
                match docker_update_cpus(&w.name, cpu_per_replica) {
                    Ok(()) => println!(
                        "[docker] {} cpus → {:.2}  ({} µs / 100ms period)",
                        w.name,
                        cpu_per_replica,
                        (cpu_per_replica * 100_000.0).round() as u64,
                    ),
                    Err(e) => eprintln!("[docker] update {} failed: {}", w.name, e),
                }
            }
        }

        self.ensure_workers(target_replicas as usize)
    }

    /// Ensure exactly `target` containers are running.
    pub fn ensure_workers(&mut self, target: usize) -> Result<()> {
        let worker_threads = (self.cpu_per_replica.ceil() as usize).max(1);

        while self.workers.len() < target {
            let idx = self.workers.len();
            let name = format!("autoscaler-worker-{}", idx);
            let url = spawn_container(
                &name,
                &self.image,
                self.cpu_factor,
                self.mem_factor,
                self.cpu_per_replica,
                worker_threads,
            )?;
            println!(
                "[docker] started {} at {}  (cpus={:.2}, worker_threads={})",
                name, url, self.cpu_per_replica, worker_threads
            );
            self.workers.push(DockerWorkerInfo { name, url });
            self.sync_backends();
        }

        while self.workers.len() > target {
            if let Some(w) = self.workers.pop() {
                println!("[docker] stopping {}", w.name);
                if let Err(e) = remove_container(&w.name) {
                    eprintln!("[docker] stop {} failed: {}", w.name, e);
                }
                self.sync_backends();
            }
        }

        Ok(())
    }

    fn sync_backends(&self) {
        let mut locked = self.backends.lock().unwrap();
        let new_urls: Vec<String> = self.workers.iter().map(|w| w.url()).collect();

        locked.retain(|e| new_urls.contains(&e.url));

        for url in &new_urls {
            if !locked.iter().any(|e| &e.url == url) {
                locked.push(BackendEntry {
                    url: url.clone(),
                    in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                });
            }
        }

        locked.sort_by_key(|e| {
            new_urls
                .iter()
                .position(|u| u == &e.url)
                .unwrap_or(usize::MAX)
        });
    }
}

impl Drop for DockerExecutor {
    fn drop(&mut self) {
        while let Some(w) = self.workers.pop() {
            let _ = remove_container(&w.name);
            println!("[docker] stopped {} (shutdown)", w.name);
        }
    }
}

// ── Docker CLI helpers ───────────────────────────────────────────────────────

/// Create the bridge network if it doesn't already exist.
fn ensure_network() -> Result<()> {
    let out = Command::new("docker")
        .args(["network", "inspect", NETWORK_NAME])
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    let out = Command::new("docker")
        .args(["network", "create", "--driver", "bridge", NETWORK_NAME])
        .output()?;
    if !out.status.success() {
        bail!(
            "docker network create failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    println!("[docker] created bridge network '{}'", NETWORK_NAME);
    Ok(())
}

/// Remove any leftover autoscaler-worker-* containers from a previous run.
fn cleanup_stale_workers() {
    let out = Command::new("docker")
        .args([
            "ps", "-a", "-q",
            "--filter", "name=autoscaler-worker",
        ])
        .output();
    if let Ok(out) = out {
        let ids = String::from_utf8_lossy(&out.stdout);
        let ids: Vec<&str> = ids.split_whitespace().collect();
        if ids.is_empty() {
            return;
        }
        println!("[docker] removing {} stale container(s) from previous run", ids.len());
        let mut cmd = Command::new("docker");
        cmd.arg("rm").arg("-f");
        for id in &ids {
            cmd.arg(id);
        }
        let _ = cmd.output();
    }
}

/// Start a worker container and return the URL reachable from the host.
///
/// We publish container port 8080 to a random host port (`-p 0:8080`) because
/// Docker container IPs are not directly routable on all platforms (Docker
/// Desktop, rootless Docker, etc.).  After the container starts we read the
/// assigned host port and return `http://127.0.0.1:<port>`.
fn spawn_container(
    name: &str,
    image: &str,
    cpu_factor: f64,
    mem_factor: f64,
    cpu_per_replica: f64,
    worker_threads: usize,
) -> Result<String> {
    // Remove any stale container with the same name before creating a new one.
    let _ = Command::new("docker").args(["rm", "-f", name]).output();

    // Memory budget: 512 MB × mem_factor, min 128 MB.
    let mem_mb = ((512.0 * mem_factor).round() as u64).max(128);
    let mem_arg = format!("{}m", mem_mb);
    let cpus_arg = format!("{:.3}", cpu_per_replica);

    let out = Command::new("docker")
        .args([
            "run", "-d",
            "--name",             name,
            "--network",          NETWORK_NAME,
            "--publish",          "0:8080",   // host assigns a random port
            "--cpus",             &cpus_arg,
            "--memory",           &mem_arg,
            "--memory-swap",      &mem_arg, // disable swap
            "--restart",          "no",
            image,
            // --- autoscaler binary args ---
            "service",
            "--port",             &WORKER_PORT.to_string(),
            "--bind-addr",        "0.0.0.0",
            "--cpu-factor",       &format!("{:.3}", cpu_factor),
            "--mem-factor",       &format!("{:.3}", mem_factor),
            "--max-concurrency",  "32",
            "--worker-threads",   &worker_threads.to_string(),
        ])
        .output()?;

    if !out.status.success() {
        bail!(
            "docker run failed for {}: {}",
            name,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    get_host_url(name)
}

/// Read the host-side port assigned by Docker (`-p 0:8080`) and return the
/// full URL reachable from the host: `http://127.0.0.1:<port>`.
/// Retries a few times because the port assignment can lag the start signal.
fn get_host_url(name: &str) -> Result<String> {
    for attempt in 0..8 {
        let out = Command::new("docker")
            .args([
                "inspect",
                "--format",
                &format!("{{{{(index (index .NetworkSettings.Ports \"{}/tcp\") 0).HostPort}}}}", WORKER_PORT),
                name,
            ])
            .output()?;

        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !text.is_empty() && text != "<no value>" {
                if let Ok(port) = text.parse::<u16>() {
                    return Ok(format!("http://127.0.0.1:{}", port));
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300 * (attempt + 1)));
    }
    bail!("could not get published port for container '{}' after retries", name)
}

/// Update a running container's CPU quota in-place (writes cgroup cpu.max under the hood).
fn docker_update_cpus(name: &str, cpu_per_replica: f64) -> Result<()> {
    let out = Command::new("docker")
        .args(["update", "--cpus", &format!("{:.3}", cpu_per_replica), name])
        .output()?;
    if !out.status.success() {
        bail!(
            "docker update --cpus failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Stop and remove a container.
fn remove_container(name: &str) -> Result<()> {
    Command::new("docker").args(["stop", "--time", "5", name]).output()?;
    Command::new("docker").args(["rm", name]).output()?;
    Ok(())
}
