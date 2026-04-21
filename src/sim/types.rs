/// Simulator-internal state types.
/// `SystemState`, `TargetState`, and `Bottleneck` live in `shared::types`; re-exported here
/// so simulator.rs and scenarios.rs can continue importing from `sim::types`.

pub use crate::shared::types::{SystemState, TargetState};

/// Used to simulate the delay between a scale-out decision and a worker becoming ready.
#[derive(Debug, Clone)]
pub struct PendingReplica {
    pub ready_at_s: u32,
}

/// Internal simulator state — separate from the observable `SystemState` the controller sees.
#[derive(Debug, Clone)]
pub struct SimulatorState {
    pub time: u32,
    pub queue: u32,
    pub active_replicas: u32,
    pub pending_replicas: Vec<PendingReplica>,
    pub cpu_per_replica: f64,
    pub mem_per_replica: f64,
}

impl Default for SimulatorState {
    fn default() -> Self {
        Self {
            time: 0,
            queue: 0,
            active_replicas: 1,
            pending_replicas: Vec::new(),
            cpu_per_replica: 1.0,
            mem_per_replica: 2.0,
        }
    }
}
