/// Unified autoscaler controller that makes both horizontal and vertical scaling decisions

use crate::metrics::Metrics;
use crate::types::{Bottleneck, SystemState, TargetState};

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
}

impl Controller {
    pub fn new() -> Self {
        Self {
            latency_target_ms: 50.0,
            cpu_util_target: 70.0,
            mem_util_target: 70.0,
            min_replicas: 1,
            max_replicas: 20,
            min_cpu: 0.5,
            max_cpu: 4.0,
            min_mem: 1.0,
            max_mem: 8.0,
        }
    }

    /// Main control loop: diagnose and scale
    pub fn decide(&self, state: &SystemState, metrics: &Metrics) -> TargetState {
        let bottleneck = self.diagnose(state, metrics);
        self.scale(state, &bottleneck)
    }

    fn diagnose(&self, state: &SystemState, metrics: &Metrics) -> Bottleneck {
        let high_latency = metrics.avg_latency_ms > self.latency_target_ms;
        let high_cpu = state.cpu_util > self.cpu_util_target;
        let high_mem = state.mem_util > self.mem_util_target;
        let has_queue = state.waiting_requests > 100;

        // Check for overprovisioning (require VERY low utilization to avoid thrashing)
        let very_low_cpu = state.cpu_util < self.cpu_util_target * 0.3;
        let very_low_mem = state.mem_util < self.mem_util_target * 0.3;
        let no_queue = state.waiting_requests == 0;

        if high_latency || has_queue {
            if high_cpu && high_mem {
                Bottleneck::Mixed
            } else if high_cpu {
                Bottleneck::Cpu
            } else if high_mem {
                Bottleneck::Memory
            } else {
                Bottleneck::Load
            }
        } else if very_low_cpu && very_low_mem && no_queue && state.num_replicas > self.min_replicas {
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
                // Need more replicas
                target_replicas = (target_replicas as f64 * 1.5).ceil() as u32;
            }
            Bottleneck::Cpu => {
                // Need more CPU per replica
                target_cpu = (target_cpu * 1.3).min(self.max_cpu);
                // If already maxed out on CPU, add replicas
                if target_cpu >= self.max_cpu {
                    target_replicas = (target_replicas as f64 * 1.2).ceil() as u32;
                }
            }
            Bottleneck::Memory => {
                // Need more memory per replica
                target_mem = (target_mem * 1.3).min(self.max_mem);
                // If already maxed out on memory, add replicas
                if target_mem >= self.max_mem {
                    target_replicas = (target_replicas as f64 * 1.2).ceil() as u32;
                }
            }
            Bottleneck::Mixed => {
                // Try both vertical and horizontal
                target_cpu = (target_cpu * 1.2).min(self.max_cpu);
                target_mem = (target_mem * 1.2).min(self.max_mem);
                target_replicas = (target_replicas as f64 * 1.3).ceil() as u32;
            }
            Bottleneck::Overprovisioned => {
                // Scale down both dimensions when severely overprovisioned
                if target_replicas > self.min_replicas {
                    // Remove 1 replica at a time (or 20% for large deployments)
                    let scale_down = if target_replicas <= 5 {
                        target_replicas - 1
                    } else {
                        ((target_replicas as f64 * 0.8).floor() as u32).max(self.min_replicas)
                    };
                    target_replicas = scale_down.max(self.min_replicas);
                }

                // Also reduce resources if they're excessive
                if target_cpu > self.min_cpu && state.cpu_util < 20.0 {
                    target_cpu = (target_cpu * 0.9).max(self.min_cpu);
                }
                if target_mem > self.min_mem && state.mem_util < 20.0 {
                    target_mem = (target_mem * 0.9).max(self.min_mem);
                }
            }
            Bottleneck::Stable | Bottleneck::Unknown => {
                // No change
            }
        }

        // Apply limits
        target_replicas = target_replicas.clamp(self.min_replicas, self.max_replicas);
        target_cpu = target_cpu.clamp(self.min_cpu, self.max_cpu);
        target_mem = target_mem.clamp(self.min_mem, self.max_mem);

        TargetState {
            num_replicas: target_replicas,
            cpu_per_replica: target_cpu,
            mem_per_replica: target_mem,
        }
    }
}
