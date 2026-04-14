/// System simulator that models request processing, queuing, and replica behavior

use crate::types::{PendingReplica, SimulatorState, SystemState, TargetState};

/// Configuration for the simulator (immutable)
#[derive(Debug, Clone)]
pub struct SimulatorConfig {
    pub requests_per_cpu_per_sec: f64,
    pub memory_per_request: f64,
    pub replica_warmup_time_s: u32,
    pub max_queue_size: u32,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            requests_per_cpu_per_sec: 100.0,
            memory_per_request: 0.01, // GB
            replica_warmup_time_s: 10,
            max_queue_size: 10000,
        }
    }
}

/// Pure function: advances simulation by one second
pub fn step(
    config: &SimulatorConfig,
    state: SimulatorState,
    target: &TargetState,
    incoming_rps: f64,
) -> (SimulatorState, SystemState) {
    let time = state.time + 1;

    // Process pending replicas that are now ready
    let mut active_replicas = state.active_replicas;
    let pending_replicas: Vec<PendingReplica> = state
        .pending_replicas
        .into_iter()
        .filter(|pending| {
            if time >= pending.ready_at_s {
                active_replicas += 1;
                false // remove from pending
            } else {
                true // keep in pending
            }
        })
        .collect();

    // Apply target configuration (vertical scaling is immediate)
    let cpu_per_replica = target.cpu_per_replica;
    let mem_per_replica = target.mem_per_replica;

    // Horizontal scaling with delay
    let mut pending_replicas = pending_replicas;
    let total_replicas = active_replicas + pending_replicas.len() as u32;
    
    if total_replicas < target.num_replicas {
        // Add pending replicas
        let to_add = target.num_replicas - total_replicas;
        for _ in 0..to_add {
            pending_replicas.push(PendingReplica {
                ready_at_s: time + config.replica_warmup_time_s,
            });
        }
    } else if total_replicas > target.num_replicas {
        // Scale down or cancel pending
        if active_replicas > target.num_replicas {
            // Scale down gradually (1 replica per second for realism)
            let can_remove = (active_replicas - target.num_replicas).min(1);
            active_replicas -= can_remove;
            pending_replicas.clear();
        } else {
            let to_remove = total_replicas - target.num_replicas;
            pending_replicas.truncate((pending_replicas.len() as u32 - to_remove) as usize);
        }
    }

    // Calculate capacity
    let capacity_per_replica = cpu_per_replica * config.requests_per_cpu_per_sec;
    let total_capacity = (active_replicas as f64 * capacity_per_replica) as u32;

    // Process queue
    let incoming = incoming_rps as u32;
    let mut queue = state.queue + incoming;
    let processed = queue.min(total_capacity);
    queue -= processed;

    // Drop requests if queue is too large
    let dropped = if queue > config.max_queue_size {
        let excess = queue - config.max_queue_size;
        queue = config.max_queue_size;
        excess
    } else {
        0
    };

    // Calculate utilization
    let cpu_util = if active_replicas > 0 && capacity_per_replica > 0.0 {
        (incoming_rps / (active_replicas as f64 * capacity_per_replica)).min(1.0) * 100.0
    } else {
        0.0
    };

    let used_memory = incoming_rps * config.memory_per_request;
    let total_memory = active_replicas as f64 * mem_per_replica;
    let mem_util = if total_memory > 0.0 {
        (used_memory / total_memory).min(1.0) * 100.0
    } else {
        0.0
    };

    let new_sim_state = SimulatorState {
        time,
        queue,
        active_replicas,
        pending_replicas,
        cpu_per_replica,
        mem_per_replica,
    };

    let system_state = SystemState {
        time_s: time,
        waiting_requests: queue,
        incoming_requests: incoming,
        incoming_rps,
        dropped_requests: dropped,
        num_replicas: active_replicas,
        cpu_per_replica,
        mem_per_replica,
        cpu_util,
        mem_util,
    };

    (new_sim_state, system_state)
}
