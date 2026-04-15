use crate::shared::workload::WorkloadPattern;
use anyhow::Result;
use reqwest::blocking::Client;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Simple local HTTP load generator using the existing WorkloadPattern
pub fn run_loadgen(
    base_url: &str,
    endpoint: &str,
    pattern: WorkloadPattern,
    duration_s: u32,
) -> Result<()> {
    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()?;

    let endpoints: Vec<String> = base_url
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| format!("{}/{}", s.trim_end_matches('/'), endpoint.trim_start_matches('/')))
        .collect();

    if endpoints.is_empty() {
        anyhow::bail!("no valid base_url provided to loadgen");
    }

    println!("Running loadgen against {:?} for {}s", endpoints, duration_s);

    let mut total_sent = 0u64;
    let mut total_errors = 0u64;
    let mut rr_index: usize = 0;

    for t in 0..duration_s {
        let rps = pattern.rps_at(t);
        let requests_this_second = rps.round().max(0.0) as u32;
        let start = Instant::now();

        for _ in 0..requests_this_second {
            let client = &client;
            let url = &endpoints[rr_index % endpoints.len()];
            rr_index = rr_index.wrapping_add(1);
            total_sent += 1;
            // Fire-and-forget style; errors are logged but do not stop the run
            if let Err(e) = client.get(url).send() {
                total_errors += 1;
                eprintln!("[loadgen] request error: {}", e);
            }
        }

        // Sleep until the end of the second to approximate the target RPS
        let elapsed = start.elapsed();
        if elapsed < Duration::from_secs(1) {
            sleep(Duration::from_secs(1) - elapsed);
        }

        if t % 10 == 0 {
            println!("[loadgen] t={}s, rps_target={:.0}, sent={} (total_sent={}, errors={})",
                t,
                rps,
                requests_this_second,
                total_sent,
                total_errors,
            );
        }
    }

    println!("[loadgen] complete: total_sent={}, errors={}", total_sent, total_errors);
    Ok(())
}

/// Convenience helpers for common patterns
pub fn steady(base_url: &str, endpoint: &str, rps: f64, duration_s: u32) -> Result<()> {
    run_loadgen(base_url, endpoint, WorkloadPattern::Steady { rps }, duration_s)
}

pub fn step(
    base_url: &str,
    endpoint: &str,
    base: f64,
    step_to: f64,
    step_at: u32,
    duration_s: u32,
) -> Result<()> {
    run_loadgen(
        base_url,
        endpoint,
        WorkloadPattern::Step {
            base,
            step_to,
            step_at,
        },
        duration_s,
    )
}

pub fn burst(
    base_url: &str,
    endpoint: &str,
    base: f64,
    peak: f64,
    burst_start: u32,
    burst_end: u32,
    duration_s: u32,
) -> Result<()> {
    run_loadgen(
        base_url,
        endpoint,
        WorkloadPattern::Burst {
            base,
            peak,
            burst_start,
            burst_end,
        },
        duration_s,
    )
}
