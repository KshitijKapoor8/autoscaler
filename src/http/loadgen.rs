use crate::shared::workload::WorkloadPattern;
use anyhow::Result;
use reqwest::blocking::Client;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Simple local HTTP load generator using the existing WorkloadPattern.
///
/// Requests are fired concurrently — each request is spawned on its own thread
/// so that the achieved RPS is not limited by per-request latency.
pub fn run_loadgen(
    base_url: &str,
    endpoint: &str,
    pattern: WorkloadPattern,
    duration_s: u32,
) -> Result<()> {
    let client = Arc::new(Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(30))
        .build()?);

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

    let total_sent   = Arc::new(AtomicU64::new(0));
    let total_errors = Arc::new(AtomicU64::new(0));
    let mut rr_index: usize = 0;

    for t in 0..duration_s {
        let rps = pattern.rps_at(t);
        let n = rps.round().max(0.0) as u32;
        let start = Instant::now();

        // Spawn n concurrent requests — don't block within the second window
        for _ in 0..n {
            let client   = client.clone();
            let url      = endpoints[rr_index % endpoints.len()].clone();
            rr_index     = rr_index.wrapping_add(1);
            let sent_ctr = total_sent.clone();
            let err_ctr  = total_errors.clone();
            std::thread::spawn(move || {
                sent_ctr.fetch_add(1, Ordering::Relaxed);
                if client.get(&url).send().is_err() {
                    err_ctr.fetch_add(1, Ordering::Relaxed);
                }
            });
        }

        // Sleep the remainder of the second so we hit ~n RPS
        let elapsed = start.elapsed();
        if elapsed < Duration::from_secs(1) {
            sleep(Duration::from_secs(1) - elapsed);
        }

        if t % 10 == 0 {
            println!(
                "[loadgen] t={}s  rps_target={:.0}  sent_this_sec={}  (total={} errors={})",
                t, rps, n,
                total_sent.load(Ordering::Relaxed),
                total_errors.load(Ordering::Relaxed),
            );
        }
    }

    println!(
        "[loadgen] complete: total_sent={}, errors={}",
        total_sent.load(Ordering::Relaxed),
        total_errors.load(Ordering::Relaxed),
    );
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
        base_url, endpoint,
        WorkloadPattern::Step { base, step_to, step_at },
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
        base_url, endpoint,
        WorkloadPattern::Burst { base, peak, burst_start, burst_end },
        duration_s,
    )
}

pub fn ramp(
    base_url: &str,
    endpoint: &str,
    base: f64,
    peak: f64,
    ramp_duration: u32,
    duration_s: u32,
) -> Result<()> {
    run_loadgen(
        base_url, endpoint,
        WorkloadPattern::Ramp { base, peak, ramp_duration },
        duration_s,
    )
}

pub fn sawtooth(
    base_url: &str,
    endpoint: &str,
    base: f64,
    peak: f64,
    period: u32,
    duration_s: u32,
) -> Result<()> {
    run_loadgen(
        base_url, endpoint,
        WorkloadPattern::Sawtooth { base, peak, period },
        duration_s,
    )
}

pub fn wave(
    base_url: &str,
    endpoint: &str,
    center: f64,
    amplitude: f64,
    period_s: u32,
    duration_s: u32,
) -> Result<()> {
    run_loadgen(
        base_url, endpoint,
        WorkloadPattern::Wave { center, amplitude, period_s },
        duration_s,
    )
}

pub fn double_burst(
    base_url: &str,
    endpoint: &str,
    base: f64,
    peak: f64,
    burst1_start: u32,
    burst1_end: u32,
    burst2_start: u32,
    burst2_end: u32,
    duration_s: u32,
) -> Result<()> {
    run_loadgen(
        base_url, endpoint,
        WorkloadPattern::DoubleBurst {
            base, peak,
            burst1_start, burst1_end,
            burst2_start, burst2_end,
        },
        duration_s,
    )
}
