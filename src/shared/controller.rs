/// Unified autoscaler controller — makes both horizontal and vertical scaling decisions.
/// Used by both the pure simulator (sim/) and the real HTTP controller (http/).

use crate::shared::metrics::Metrics;
use crate::shared::types::{Bottleneck, SystemState, TargetState};

pub struct Controller {
    pub latency_target_ms: f64,
    pub cpu_util_target: f64,
    pub mem_util_target: f64,
    pub min_replicas: u32,
    pub max_replicas: u32,
    pub min_cpu: f64,
    pub max_cpu: f64,
    pub min_mem: f64,
    pub max_mem: f64,
    /// Concurrency slots per replica (= max_concurrency in service.rs).
    /// Used for demand-based replica math: needed = ceil((queue + active) / slots_per_replica).
    pub slots_per_replica: u32,
    /// Queue depth above which the caller should bypass the normal control interval
    /// and run decide() immediately (emergency fast path).
    pub emergency_queue_threshold: u32,
}

impl Controller {
    pub fn new() -> Self {
        Self {
            latency_target_ms: 400.0,
            cpu_util_target: 70.0,
            mem_util_target: 70.0,
            min_replicas: 1,
            max_replicas: 20,
            min_cpu: 0.5,
            max_cpu: 4.0,
            min_mem: 1.0,
            max_mem: 8.0,
            slots_per_replica: 32,
            emergency_queue_threshold: 20,
        }
    }

    /// Returns true when conditions are severe enough to bypass the normal interval.
    pub fn is_emergency(&self, state: &SystemState) -> bool {
        state.waiting_requests >= self.emergency_queue_threshold
    }

    /// Main control loop: diagnose and scale.
    pub fn decide(&self, state: &SystemState, metrics: &Metrics) -> TargetState {
        let bottleneck = self.diagnose(state, metrics);
        self.scale(state, &bottleneck)
    }

    fn diagnose(&self, state: &SystemState, metrics: &Metrics) -> Bottleneck {
        let high_latency = metrics.avg_latency_ms > self.latency_target_ms;
        let high_cpu = state.cpu_util > self.cpu_util_target;
        let high_mem = state.mem_util > self.mem_util_target;
        let has_queue = state.waiting_requests > 0;

        // The rolling latency window is a trailing average over the last 100 completed requests.
        // After a burst, it stays high for minutes even when the system is completely idle.
        // Only treat high latency as evidence of a problem when there is also current load;
        // otherwise the stale window blocks scale-down.
        let latency_under_load =
            high_latency && (high_cpu || high_mem || has_queue || state.cpu_util > 10.0);

        let very_low_cpu = state.cpu_util < self.cpu_util_target * 0.3;
        let very_low_mem = state.mem_util < self.mem_util_target * 0.3;
        let no_queue = state.waiting_requests == 0;

        if latency_under_load || has_queue {
            if high_cpu && high_mem {
                Bottleneck::Mixed
            } else if high_cpu {
                Bottleneck::Cpu
            } else if high_mem {
                Bottleneck::Memory
            } else {
                Bottleneck::Load
            }
        } else if very_low_cpu && very_low_mem && no_queue && state.num_replicas > self.min_replicas
        {
            Bottleneck::Overprovisioned
        } else if !high_cpu && !high_mem && !has_queue {
            Bottleneck::Stable
        } else {
            Bottleneck::Unknown
        }
    }

    fn scale(&self, state: &SystemState, bottleneck: &Bottleneck) -> TargetState {
        let mut target_replicas = state.num_replicas;
        let mut target_cpu = state.cpu_per_replica;
        let mut target_mem = state.mem_per_replica;

        match bottleneck {
            Bottleneck::Load => {
                target_replicas = self.replicas_for_demand(state);
            }
            Bottleneck::Cpu => {
                target_cpu = (target_cpu * 1.3).min(self.max_cpu);
                let demand_replicas = self.replicas_for_demand(state);
                if demand_replicas > target_replicas || target_cpu >= self.max_cpu {
                    target_replicas = demand_replicas;
                }
            }
            Bottleneck::Memory => {
                target_mem = (target_mem * 1.3).min(self.max_mem);
                let demand_replicas = self.replicas_for_demand(state);
                if demand_replicas > target_replicas || target_mem >= self.max_mem {
                    target_replicas = demand_replicas;
                }
            }
            Bottleneck::Mixed => {
                target_cpu = (target_cpu * 1.2).min(self.max_cpu);
                target_mem = (target_mem * 1.2).min(self.max_mem);
                target_replicas = self.replicas_for_demand(state);
            }
            Bottleneck::Overprovisioned => {
                if target_replicas > self.min_replicas {
                    let scale_down = if target_replicas <= 5 {
                        target_replicas - 1
                    } else {
                        ((target_replicas as f64 * 0.8).floor() as u32).max(self.min_replicas)
                    };
                    target_replicas = scale_down.max(self.min_replicas);
                }
                if target_cpu > self.min_cpu && state.cpu_util < 20.0 {
                    target_cpu = (target_cpu * 0.9).max(self.min_cpu);
                }
                if target_mem > self.min_mem && state.mem_util < 20.0 {
                    target_mem = (target_mem * 0.9).max(self.min_mem);
                }
            }
            Bottleneck::Stable | Bottleneck::Unknown => {}
        }

        target_replicas = target_replicas.clamp(self.min_replicas, self.max_replicas);
        target_cpu = target_cpu.clamp(self.min_cpu, self.max_cpu);
        target_mem = target_mem.clamp(self.min_mem, self.max_mem);

        TargetState {
            num_replicas: target_replicas,
            cpu_per_replica: target_cpu,
            mem_per_replica: target_mem,
        }
    }

    /// How many replicas are needed to absorb the current queue + active load?
    ///
    /// Formula: ceil((queued + active_slots_in_use) / slots_per_replica)
    /// where active_slots_in_use ≈ cpu_util% × current_total_slots.
    ///
    /// Growth is capped at 3× current replicas per decision to prevent overshoot.
    fn replicas_for_demand(&self, state: &SystemState) -> u32 {
        let total_slots = state.num_replicas * self.slots_per_replica;
        let active_slots = (state.cpu_util / 100.0 * total_slots as f64).round() as u32;
        let total_demand = state.waiting_requests + active_slots;

        if total_demand == 0 {
            return state.num_replicas;
        }

        let needed = (total_demand as f64 / self.slots_per_replica as f64).ceil() as u32;
        needed
            .max(state.num_replicas)
            .min(state.num_replicas * 3)
            .max(1)
    }
}
