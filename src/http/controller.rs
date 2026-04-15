use crate::http::executor::Executor;
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
    in_flight: u64,
    errors: u64,
    avg_latency_ms: f64,
}

fn fetch_instance_metrics(client: &Client, base_url: &str) -> Result<InstanceMetrics> {
    let url = format!("{}/metrics", base_url.trim_end_matches('/'));
    let body = client.get(&url).send()?.text()?;

    let mut m = InstanceMetrics::default();
    for line in body.lines() {
        let mut parts = line.split('=');
        let key = parts.next().unwrap_or("").trim();
        let val = parts.next().unwrap_or("").trim();
        if val.is_empty() {
            continue;
        }
        match key {
            "requests_total" => m.total_requests = val.parse().unwrap_or(0),
            "in_flight" => m.in_flight = val.parse().unwrap_or(0),
            "errors_total" => m.errors = val.parse().unwrap_or(0),
            "avg_latency_ms" => m.avg_latency_ms = val.parse().unwrap_or(0.0),
            _ => {}
        }
    }

    Ok(m)
}

/// Run a simple closed-loop controller against the real HTTP workers.
///
/// This reuses the Phase 1 Controller but drives it from real /metrics.
pub fn run_http_controller(
    base_port: u16,
    initial_replicas: u32,
    duration_s: u32,
    control_interval_s: u32,
) -> Result<()> {
    let client = Client::builder().build()?;
    let mut target = TargetState::default();
    let mut executor = Executor::new(base_port, target.cpu_per_replica, target.mem_per_replica);
    executor.ensure_workers(initial_replicas as usize)?;

    let mut controller = Controller::new();

    let mut last_total_requests: u64 = 0;

    for t in 0..duration_s {
        let urls = executor.worker_urls();
        if urls.is_empty() {
            // Ensure at least one worker is running
            executor.ensure_workers(1)?;
        }

        // Aggregate metrics across all instances
        let mut agg = InstanceMetrics::default();
        let mut instances = 0u64;
        for url in &urls {
            match fetch_instance_metrics(&client, url) {
                Ok(m) => {
                    agg.total_requests += m.total_requests;
                    agg.in_flight += m.in_flight;
                    agg.errors += m.errors;
                    agg.avg_latency_ms += m.avg_latency_ms;
                    instances += 1;
                }
                Err(e) => {
                    eprintln!("[http-ctl] failed to fetch metrics from {}: {}", url, e);
                }
            }
        }

        if instances > 0 {
            agg.avg_latency_ms /= instances as f64;
        }

        let delta_requests = agg.total_requests.saturating_sub(last_total_requests);
        last_total_requests = agg.total_requests;
        let incoming_rps = delta_requests as f64;

        // Map real metrics into the simulated SystemState the controller expects
        let mut state = SystemState::default();
        state.time_s = t;
        state.incoming_requests = delta_requests as u32;
        state.incoming_rps = incoming_rps;
        state.waiting_requests = agg.in_flight as u32;
        state.num_replicas = urls.len() as u32;
        state.cpu_per_replica = target.cpu_per_replica;
        state.mem_per_replica = target.mem_per_replica;

        // Crude utilization estimates: treat latency + in-flight as load signal
        state.cpu_util = (agg.avg_latency_ms / 10.0 * 10.0).clamp(1.0, 100.0);
        state.mem_util = (agg.in_flight as f64 * 10.0).clamp(1.0, 100.0);

        let metrics = SimMetrics::from_state(&state);

        if t % control_interval_s == 0 && t > 0 {
            let new_target = controller.decide(&state, &metrics);

            let changed = new_target.num_replicas != target.num_replicas
                || (new_target.cpu_per_replica - target.cpu_per_replica).abs() > 0.01
                || (new_target.mem_per_replica - target.mem_per_replica).abs() > 0.01;

            if changed {
                executor.apply_target(
                    new_target.num_replicas,
                    new_target.cpu_per_replica,
                    new_target.mem_per_replica,
                )?;
                println!(
                    "[http-ctl] t={}s, scale → R:{} CPU:{:.1} Mem:{:.1} (prev R:{} CPU:{:.1} Mem:{:.1})",
                    t,
                    new_target.num_replicas,
                    new_target.cpu_per_replica,
                    new_target.mem_per_replica,
                    target.num_replicas,
                    target.cpu_per_replica,
                    target.mem_per_replica,
                );
            }

            target = new_target;
        }

        if t % 5 == 0 {
            println!(
                "[http-ctl] t={}s, rps={:.1}, replicas={}, in_flight={}, avg_latency_ms={:.1}",
                t,
                incoming_rps,
                state.num_replicas,
                state.waiting_requests,
                agg.avg_latency_ms,
            );
        }

        sleep(Duration::from_secs(1));
    }

    Ok(())
}
