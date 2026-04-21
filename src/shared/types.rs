/// Core types shared by both the simulator and the HTTP controller.

/// Observable system state — fed into the controller on every tick.
#[derive(Debug, Default, Clone)]
pub struct SystemState {
    pub time_s: u32,
    pub waiting_requests: u32,
    pub incoming_requests: u32,
    pub incoming_rps: f64,
    pub dropped_requests: u32,
    pub num_replicas: u32,
    pub cpu_per_replica: f64,
    pub mem_per_replica: f64,
    pub cpu_util: f64,
    pub mem_util: f64,
}

/// The desired target the controller wants the system to converge toward.
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

/// What the controller diagnosed as the current system bottleneck.
pub enum Bottleneck {
    /// Total incoming demand too high for current replica count — horizontal scaling needed.
    Load,
    /// Replicas are individually CPU-bound — vertical CPU scaling needed.
    Cpu,
    /// Replicas are individually memory-bound — vertical memory scaling needed.
    Memory,
    /// Multiple bottlenecks present simultaneously.
    Mixed,
    /// System has more resources than needed — scale down.
    Overprovisioned,
    /// System is healthy and right-sized.
    Stable,
    /// State cannot be determined from available signals.
    Unknown,
}
