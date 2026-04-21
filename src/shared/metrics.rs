/// Latency and queue metrics fed into the controller's decision logic.

use crate::shared::types::SystemState;

pub struct Metrics {
    pub avg_latency_ms: f64,
    pub p99_latency_ms: f64,
    pub queue_depth: u32,
}

impl Metrics {
    /// Derive metrics from simulated system state using a simple queue-latency model.
    /// The HTTP controller bypasses this and builds `Metrics` directly from real measurements.
    pub fn from_state(state: &SystemState) -> Self {
        let processing_time_ms = 10.0;
        let capacity = state.num_replicas as f64 * state.cpu_per_replica * 100.0;
        let queue_wait_s = if capacity > 0.0 {
            state.waiting_requests as f64 / capacity
        } else {
            0.0
        };
        let avg_latency_ms = processing_time_ms + (queue_wait_s * 1000.0);
        let p99_latency_ms = if state.waiting_requests > 0 {
            avg_latency_ms * 2.0
        } else {
            avg_latency_ms
        };
        Metrics { avg_latency_ms, p99_latency_ms, queue_depth: state.waiting_requests }
    }
}
