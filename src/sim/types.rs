/// Global System State
#[derive(Debug, Default, Clone)]
pub struct SystemState {
    /// Request metrics
    pub time_s: u32,
    pub waiting_requests: u32,
    pub incoming_requests: u32,
    pub incoming_rps: f64,
    pub dropped_requests: u32,

    /// Scaling dimensions
    pub num_replicas: u32,
    pub cpu_per_replica: f64,
    pub mem_per_replica: f64,

    /// Current load metrics
    pub cpu_util: f64,
    pub mem_util: f64,
}

/// Used to simulate delay in worker start
#[derive(Debug, Clone)]
pub struct PendingReplica {
    pub ready_at_s: u32,
}

/// Internal simulator state (separate from observable SystemState)
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

/// The goal system scaling that the controller must move toward
#[derive(Debug, Clone)]
pub struct TargetState {
    pub num_replicas: u32,
    pub cpu_per_replica: f64,
    pub mem_per_replica: f64,
}

impl Default for TargetState {
    fn default() -> Self {
        Self {
            num_replicas: 1,
            cpu_per_replica: 1.0,
            mem_per_replica: 2.0,
        }
    }
}

pub enum Bottleneck {
    /// Total incoming demand too high for current replica count
    ///
    /// Implies horizontal scaling needed
    Load,
    /// Replicas are individually struggling
    ///
    /// Implies vertical scaling needed (increasing cpu_per_replica)
    Cpu,
    /// Replicas are individually running low on memory
    ///
    /// Implies vertical scaling needed (increasing mem_per_replica)
    Memory,
    /// Multiple bottlenecks present
    Mixed,
    /// Requests are being handled easily, the system has more resources than needed
    Overprovisioned,
    /// System is stable, not wasting resources and handling all requests
    Stable,
    /// Default state. Cannot determine current state.
    Unknown,
}
