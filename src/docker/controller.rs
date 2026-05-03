use crate::docker::executor::DockerExecutor;
use crate::http::proxy::start_proxy;
use crate::shared::controller::Controller;
use crate::shared::metrics::Metrics;
use crate::shared::types::{SystemState, TargetState};
use anyhow::Result;
use reqwest::blocking::Client;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Per-instance metrics (same wire format as http workers) ─────────────────

#[derive(Debug, Default, Clone)]
struct InstanceMetrics {
    total_requests: u64,
    errors: u64,
    avg_latency_ms: f64,
    max_concurrency: usize,
    active_count: usize,
    queue_depth: usize,
    os_cpu_ticks: u64,
    rss_kb: u64,
}

fn fetch_instance_metrics(client: &Client, base_url: &str) -> Result<InstanceMetrics> {
    let url = format!("{}/metrics", base_url.trim_end_matches('/'));
    let body = client.get(&url).timeout(Duration::from_secs(3)).send()?.text()?;

    let mut m = InstanceMetrics::default();
    for line in body.lines() {
        let mut parts = line.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim();
        let val = parts.next().unwrap_or("").trim();
        if val.is_empty() { continue; }
        match key {
            "requests_total"  => m.total_requests  = val.parse().unwrap_or(0),
            "errors_total"    => m.errors           = val.parse().unwrap_or(0),
            "avg_latency_ms"  => m.avg_latency_ms   = val.parse().unwrap_or(0.0),
            "max_concurrency" => m.max_concurrency   = val.parse().unwrap_or(1),
            "active_count"    => m.active_count      = val.parse().unwrap_or(0),
            "queue_depth"     => m.queue_depth       = val.parse().unwrap_or(0),
            "os_cpu_ticks"    => m.os_cpu_ticks      = val.parse().unwrap_or(0),
            "rss_kb"          => m.rss_kb            = val.parse().unwrap_or(0),
            _ => {}
        }
    }
    Ok(m)
}

// ── CSV output ───────────────────────────────────────────────────────────────

struct CsvOutput {
    summary: BufWriter<File>,
    workers: BufWriter<File>,
}

impl CsvOutput {
    fn open(csv_dir: &str) -> Result<Self> {
        fs::create_dir_all(csv_dir)?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let summary_path = PathBuf::from(csv_dir).join(format!("docker_ctl_{}_summary.csv", ts));
        let workers_path = PathBuf::from(csv_dir).join(format!("docker_ctl_{}_workers.csv", ts));

        let mut summary = BufWriter::new(File::create(&summary_path)?);
        let mut workers = BufWriter::new(File::create(&workers_path)?);

        writeln!(
            summary,
            "t_s,num_workers,rps,active_total,capacity_total,queued_total,\
             avg_latency_ms,cpu_util_pct,cpu_per_worker,errors_total,os_cpu_pct_total,rss_mb_total,scale_event"
        )?;
        // "container" instead of "port" — worker is identified by container name
        writeln!(workers, "t_s,container,cpu_per_replica,active,capacity,queued,os_cpu_pct,rss_mb")?;

        println!("[docker-ctl] CSV summary → {}", summary_path.display());
        println!("[docker-ctl] CSV workers → {}", workers_path.display());

        Ok(CsvOutput { summary, workers })
    }

    #[allow(clippy::too_many_arguments)]
    fn write_summary(
        &mut self,
        t: u32, num_workers: usize, rps: f64,
        active: usize, capacity: usize, queued: usize,
        latency_ms: f64, cpu_util_pct: f64, cpu_per_worker: f64,
        errors_total: u64, os_cpu_pct_total: f64, rss_mb_total: f64,
        scale_event: &str,
    ) {
        let _ = writeln!(
            self.summary,
            "{},{},{:.2},{},{},{},{:.2},{:.1},{:.3},{},{:.1},{:.1},{}",
            t, num_workers, rps, active, capacity, queued,
            latency_ms, cpu_util_pct, cpu_per_worker,
            errors_total, os_cpu_pct_total, rss_mb_total, scale_event
        );
    }

    fn write_worker(
        &mut self,
        t: u32, container: &str, cpu_per_replica: f64,
        active: usize, capacity: usize, queued: usize,
        os_cpu_pct: f64, rss_mb: f64,
    ) {
        let _ = writeln!(
            self.workers,
            "{},{},{:.3},{},{},{},{:.1},{:.1}",
            t, container, cpu_per_replica, active, capacity, queued, os_cpu_pct, rss_mb
        );
    }

    fn flush(&mut self) {
        let _ = self.summary.flush();
        let _ = self.workers.flush();
    }
}

// ── Main control loop ────────────────────────────────────────────────────────

/// Run the Docker-backed controller.
///
/// Workers are Docker containers on the `autoscaler-net` bridge network.
/// The proxy (same TCP proxy as the OS-process path) runs on `proxy_port`
/// on the host and forwards to container IPs.
///
/// Vertical scaling = `docker update --cpus` (in-place, no restart).
/// Horizontal scaling = `docker run` / `docker stop+rm`.
///
/// `policy` controls which scaling axes are active:
///   "unified"  — both horizontal and vertical (default)
///   "hpa-only" — horizontal only; cpu_per_replica is never changed
///   "vpa-only" — vertical only; num_replicas is never changed
pub fn run_docker_controller(
    proxy_port: u16,
    image: &str,
    initial_replicas: u32,
    duration_s: u32,
    control_interval_s: u32,
    cpu_factor: f64,
    mem_factor: f64,
    policy: &str,
    csv_dir: Option<&str>,
) -> Result<()> {
    let client = Client::builder().build()?;
    let mut target = TargetState::default();

    let mut executor = DockerExecutor::new(image, cpu_factor, mem_factor, target.cpu_per_replica)?;
    executor.ensure_workers(initial_replicas as usize)?;

    // Proxy runs on the host; it can reach container IPs directly on Linux.
    start_proxy(proxy_port, executor.backends.clone())?;
    println!("[docker-ctl] proxy   → http://127.0.0.1:{}", proxy_port);
    println!("[docker-ctl] network → autoscaler-net (bridge)");
    println!("[docker-ctl] image   → {}", image);
    println!("[docker-ctl] policy  → {}", policy);
    println!("[docker-ctl] point your loadgen at http://127.0.0.1:{}", proxy_port);

    let mut csv = csv_dir.map(|dir| CsvOutput::open(dir).expect("failed to open CSV"));

    let mut controller = Controller::new();
    let mut last_total_requests: u64 = 0;
    let mut last_decision_t: u32 = 0;
    let mut prev_cpu_ticks: HashMap<String, u64> = HashMap::new();

    for t in 0..duration_s {
        let worker_states = executor.worker_states();
        if worker_states.is_empty() {
            executor.ensure_workers(1)?;
        }

        // ── collect per-worker metrics ─────────────────────────────────────
        let mut per_worker: Vec<(String, f64, InstanceMetrics)> = Vec::new();
        let mut agg = InstanceMetrics::default();
        let mut instances = 0usize;

        for ws in &worker_states {
            match fetch_instance_metrics(&client, &ws.url) {
                Ok(m) => {
                    agg.total_requests  += m.total_requests;
                    agg.errors          += m.errors;
                    agg.avg_latency_ms  += m.avg_latency_ms;
                    agg.max_concurrency += m.max_concurrency;
                    agg.active_count    += m.active_count;
                    agg.queue_depth     += m.queue_depth;

                    let prev = prev_cpu_ticks.get(&ws.name).copied().unwrap_or(m.os_cpu_ticks);
                    agg.os_cpu_ticks += m.os_cpu_ticks.saturating_sub(prev);
                    prev_cpu_ticks.insert(ws.name.clone(), m.os_cpu_ticks);
                    agg.rss_kb += m.rss_kb;

                    instances += 1;
                    per_worker.push((ws.name.clone(), ws.cpu_quota, m));
                }
                Err(e) => eprintln!("[docker-ctl] metrics fetch {} ({}): {}", ws.name, ws.url, e),
            }
        }
        if instances > 0 {
            agg.avg_latency_ms /= instances as f64;
        }

        let os_cpu_pct_total = agg.os_cpu_ticks as f64;
        let rss_mb_total     = agg.rss_kb as f64 / 1024.0;
        let delta = agg.total_requests.saturating_sub(last_total_requests);
        last_total_requests = agg.total_requests;

        // ── build SystemState ──────────────────────────────────────────────
        let mut state = SystemState::default();
        state.time_s            = t;
        state.incoming_rps      = delta as f64;
        state.incoming_requests = delta as u32;
        state.waiting_requests  = agg.queue_depth as u32;
        state.num_replicas      = worker_states.len() as u32;
        state.cpu_per_replica   = target.cpu_per_replica;
        state.mem_per_replica   = target.mem_per_replica;

        state.cpu_util = if agg.max_concurrency > 0 {
            (agg.active_count as f64 / agg.max_concurrency as f64 * 100.0).clamp(1.0, 100.0)
        } else { 1.0 };

        state.mem_util = if agg.max_concurrency > 0 {
            (agg.queue_depth as f64 / agg.max_concurrency as f64 * 100.0).clamp(1.0, 100.0)
        } else { 1.0 };

        let metrics = Metrics {
            avg_latency_ms: agg.avg_latency_ms,
            p99_latency_ms: agg.avg_latency_ms * 1.5,
            queue_depth:    agg.queue_depth as u32,
        };

        // ── control decision ───────────────────────────────────────────────
        let time_since_last = t.saturating_sub(last_decision_t);
        let emergency   = controller.is_emergency(&state) && time_since_last >= 3;
        let normal_tick = t % control_interval_s == 0 && t > 0;
        let mut scale_event = String::from("none");

        if normal_tick || emergency {
            last_decision_t = t;
            let mut new_target = controller.decide(&state, &metrics);

            // Apply policy constraints: lock out the unused scaling axis.
            match policy {
                "hpa-only" => new_target.cpu_per_replica = target.cpu_per_replica,
                "vpa-only" => new_target.num_replicas    = target.num_replicas,
                _          => {} // "unified" — use both axes
            }

            let replicas_changed    = new_target.num_replicas != target.num_replicas;
            let concurrency_changed = (new_target.cpu_per_replica - target.cpu_per_replica).abs() > 0.01;

            if replicas_changed || concurrency_changed {
                executor.apply_target(
                    new_target.num_replicas,
                    new_target.cpu_per_replica,
                    new_target.mem_per_replica,
                )?;

                let mut parts = Vec::new();
                if replicas_changed {
                    let dir = if new_target.num_replicas > target.num_replicas { "scale_out" } else { "scale_in" };
                    parts.push(format!("{}:{}→{}", dir, target.num_replicas, new_target.num_replicas));
                }
                if concurrency_changed {
                    let dir = if new_target.cpu_per_replica > target.cpu_per_replica { "cpu_up" } else { "cpu_down" };
                    parts.push(format!("{}:{:.2}→{:.2}", dir, target.cpu_per_replica, new_target.cpu_per_replica));
                }
                scale_event = parts.join("|");

                println!(
                    "[docker-ctl] t={}s  SCALE{}  replicas: {} → {}  cpu/worker: {:.2} → {:.2}",
                    t,
                    if emergency { " [emergency]" } else { "" },
                    target.num_replicas, new_target.num_replicas,
                    target.cpu_per_replica, new_target.cpu_per_replica,
                );
            }

            target = new_target;
        }

        // ── console status every 5s ────────────────────────────────────────
        if t % 5 == 0 {
            println!(
                "[docker-ctl] t={}s  workers={}  rps={:.1}  active={}/{}  queued={}  \
                 avg_latency_ms={:.1}  os_cpu={:.1}%  rss={:.1}MB",
                t, worker_states.len(), delta as f64,
                agg.active_count, agg.max_concurrency,
                agg.queue_depth, agg.avg_latency_ms,
                os_cpu_pct_total, rss_mb_total,
            );
            for (name, cpu, m) in &per_worker {
                let prev = prev_cpu_ticks.get(name).copied().unwrap_or(0);
                let worker_cpu = m.os_cpu_ticks.saturating_sub(prev) as f64;
                println!(
                    "[docker-ctl]   {} cpu_quota={:.2} os_cpu={:.1}% rss={:.1}MB",
                    name, cpu, worker_cpu, m.rss_kb as f64 / 1024.0
                );
            }
        }

        // ── CSV every second ───────────────────────────────────────────────
        if let Some(ref mut out) = csv {
            out.write_summary(
                t, worker_states.len(), delta as f64,
                agg.active_count, agg.max_concurrency, agg.queue_depth,
                agg.avg_latency_ms, state.cpu_util, target.cpu_per_replica,
                agg.errors, os_cpu_pct_total, rss_mb_total, &scale_event,
            );
            for (name, cpu_quota, m) in &per_worker {
                let prev = prev_cpu_ticks.get(name).copied().unwrap_or(m.os_cpu_ticks);
                let worker_cpu = m.os_cpu_ticks.saturating_sub(prev) as f64;
                out.write_worker(
                    t, name, *cpu_quota,
                    m.active_count, m.max_concurrency, m.queue_depth,
                    worker_cpu, m.rss_kb as f64 / 1024.0,
                );
            }
            out.flush();
        }

        sleep(Duration::from_secs(1));
    }

    Ok(())
}
