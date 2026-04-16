use crate::http::executor::Executor;
use crate::http::proxy::start_proxy;
use crate::sim::controller::Controller;
use crate::sim::metrics::Metrics as SimMetrics;
use crate::sim::types::{SystemState, TargetState};
use anyhow::Result;
use reqwest::blocking::Client;
use std::thread::sleep;
use std::time::Duration;

#[derive(Debug, Default, Clone)]
struct InstanceMetrics {
    total_requests: u64,
    errors: u64,
    avg_latency_ms: f64,
    max_concurrency: usize,
    active_count: usize,
    queue_depth: usize,
}

fn fetch_instance_metrics(client: &Client, base_url: &str) -> Result<InstanceMetrics> {
    let url = format!("{}/metrics", base_url.trim_end_matches('/'));
    let body = client.get(&url).send()?.text()?;

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
            _ => {}
        }
    }
    Ok(m)
}

/// Run the HTTP controller.
///
/// - Starts a proxy on `proxy_port` as the stable frontend (point loadgen here).
/// - Manages worker processes on `base_port`, `base_port+1`, …
/// - `cpu_factor` / `mem_factor` set how heavy each request is (workload type, fixed per run).
/// - The controller adjusts `num_replicas` (horizontal) and `max_concurrency` (vertical).
pub fn run_http_controller(
    base_port: u16,
    proxy_port: u16,
    initial_replicas: u32,
    duration_s: u32,
    control_interval_s: u32,
    cpu_factor: f64,
    mem_factor: f64,
) -> Result<()> {
    let client = Client::builder().build()?;
    let mut target = TargetState::default();

    let mut executor = Executor::new(base_port, cpu_factor, mem_factor, target.cpu_per_replica);
    executor.ensure_workers(initial_replicas as usize)?;

    // Proxy on a stable port so loadgen never needs to know about individual workers
    start_proxy(proxy_port, executor.backends.clone())?;
    println!("[http-ctl] proxy  → http://127.0.0.1:{}", proxy_port);
    println!("[http-ctl] workers → :{}+", base_port);
    println!("[http-ctl] point your loadgen at http://127.0.0.1:{}", proxy_port);

    let mut controller = Controller::new();
    let mut last_total_requests: u64 = 0;

    for t in 0..duration_s {
        let urls = executor.worker_urls();
        if urls.is_empty() {
            executor.ensure_workers(1)?;
        }

        // Aggregate metrics across all workers
        let mut agg = InstanceMetrics::default();
        let mut instances = 0usize;
        for url in &urls {
            match fetch_instance_metrics(&client, url) {
                Ok(m) => {
                    agg.total_requests  += m.total_requests;
                    agg.errors          += m.errors;
                    agg.avg_latency_ms  += m.avg_latency_ms;
                    agg.max_concurrency += m.max_concurrency;
                    agg.active_count    += m.active_count;
                    agg.queue_depth     += m.queue_depth;
                    instances += 1;
                }
                Err(e) => eprintln!("[http-ctl] metrics fetch failed {}: {}", url, e),
            }
        }
        if instances > 0 {
            agg.avg_latency_ms /= instances as f64;
        }

        let delta = agg.total_requests.saturating_sub(last_total_requests);
        last_total_requests = agg.total_requests;

        // Build SystemState with real signals
        let mut state = SystemState::default();
        state.time_s         = t;
        state.incoming_rps   = delta as f64;
        state.incoming_requests = delta as u32;
        state.waiting_requests  = agg.queue_depth as u32;  // real queue depth
        state.num_replicas      = urls.len() as u32;
        state.cpu_per_replica   = target.cpu_per_replica;
        state.mem_per_replica   = target.mem_per_replica;

        // Real utilization: fraction of concurrency slots actually in use
        state.cpu_util = if agg.max_concurrency > 0 {
            (agg.active_count as f64 / agg.max_concurrency as f64 * 100.0).clamp(1.0, 100.0)
        } else { 1.0 };

        // Memory pressure: queue relative to total concurrency capacity
        state.mem_util = if agg.max_concurrency > 0 {
            (agg.queue_depth as f64 / agg.max_concurrency as f64 * 100.0).clamp(1.0, 100.0)
        } else { 1.0 };

        // Use real measured latency rather than SimMetrics' fake queue model
        let metrics = SimMetrics {
            avg_latency_ms: agg.avg_latency_ms,
            p99_latency_ms: agg.avg_latency_ms * 1.5,
            queue_depth: agg.queue_depth as u32,
        };

        if t % control_interval_s == 0 && t > 0 {
            let new_target = controller.decide(&state, &metrics);

            let replicas_changed    = new_target.num_replicas != target.num_replicas;
            let concurrency_changed = (new_target.cpu_per_replica - target.cpu_per_replica).abs() > 0.01;

            if replicas_changed || concurrency_changed {
                executor.apply_target(
                    new_target.num_replicas,
                    new_target.cpu_per_replica,
                    new_target.mem_per_replica,
                )?;
                println!(
                    "[http-ctl] t={}s  SCALE  replicas: {} → {}  cpu/worker: {:.2} → {:.2}",
                    t,
                    target.num_replicas, new_target.num_replicas,
                    target.cpu_per_replica, new_target.cpu_per_replica,
                );
            }

            target = new_target;
        }

        if t % 5 == 0 {
            let worker_states = executor.worker_states();
            println!(
                "[http-ctl] t={}s  workers={}  rps={:.1}  active={}/{}  queued={}  avg_latency_ms={:.1}",
                t, worker_states.len(), delta as f64,
                agg.active_count, agg.max_concurrency,
                agg.queue_depth, agg.avg_latency_ms,
            );
            for (port, cpu) in &worker_states {
                println!("[http-ctl]   worker :{} cpu={:.2}", port, cpu);
            }
        }

        sleep(Duration::from_secs(1));
    }

    Ok(())
}
