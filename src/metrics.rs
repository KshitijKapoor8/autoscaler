/// Derives metrics from system state

use crate::types::SystemState;

pub struct Metrics {
    pub avg_latency_ms: f64,
    pub p99_latency_ms: f64,
    pub queue_depth: u32,
}

impl Metrics {
    pub fn from_state(state: &SystemState) -> Self {
        // Simple latency model: latency increases with queue depth
        // Assume each request takes ~10ms to process
        let processing_time_ms = 10.0;
        
        // Average latency = processing time + queue wait time
        // If capacity is C req/s and queue is Q, average wait = Q/C seconds
        let capacity = state.num_replicas as f64 * state.cpu_per_replica * 100.0; // requests/sec
        
        let queue_wait_s = if capacity > 0.0 {
            state.waiting_requests as f64 / capacity
        } else {
            0.0
        };
        
        let avg_latency_ms = processing_time_ms + (queue_wait_s * 1000.0);
        
        // Simple p99 estimate: 2x average when under load
        let p99_latency_ms = if state.waiting_requests > 0 {
            avg_latency_ms * 2.0
        } else {
            avg_latency_ms
        };

        Metrics {
            avg_latency_ms,
            p99_latency_ms,
            queue_depth: state.waiting_requests,
        }
    }
}
