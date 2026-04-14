/// Example simulation scenarios with different workload patterns

use crate::controller::Controller;
use crate::metrics::Metrics;
use crate::simulator::{step, SimulatorConfig};
use crate::types::{SimulatorState, TargetState};
use crate::workload::WorkloadPattern;
use std::fs::{self, File};
use std::io::Write;

pub struct OutputConfig {
    pub console: bool,
    pub csv: bool,
    pub csv_dir: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            console: true,
            csv: false,
            csv_dir: String::from("data"),
        }
    }
}

pub fn run_scenario(name: &str, workload: WorkloadPattern, duration: u32, output: &OutputConfig) {
    println!("\nScenario: {}\n", name);

    let config = SimulatorConfig::default();
    let controller = Controller::new();
    let control_interval = 10;

    let mut sim_state = SimulatorState::default();
    let mut target = TargetState::default();

    // Prepare CSV output if enabled
    let mut csv_file = if output.csv {
        fs::create_dir_all(&output.csv_dir).expect("Failed to create data directory");
        let filename = format!("{}/{}.csv", output.csv_dir, sanitize_filename(name));
        let file = File::create(&filename).expect("Failed to create CSV file");
        println!("Writing CSV to: {}\n", filename);
        Some(file)
    } else {
        None
    };

    // Write CSV header
    if let Some(ref mut file) = csv_file {
        writeln!(
            file,
            "time_s,rps,queue,replicas,cpu_per_replica,mem_per_replica,cpu_util,mem_util,latency_ms,action"
        )
        .expect("Failed to write CSV header");
    }

    // Print console header
    if output.console {
        println!("Time(s) | RPS   | Queue | Replicas | CPU/Rep | Mem/Rep | CPU% | Mem% | Latency(ms) | Action");
        println!("{}", "-".repeat(100));
    }

    let mut data_points = Vec::new();

    for t in 0..duration {
        let incoming_rps = workload.rps_at(t);
        
        let (new_sim_state, system_state) = step(&config, sim_state, &target, incoming_rps);
        sim_state = new_sim_state;
        
        let metrics = Metrics::from_state(&system_state);

        let mut action = String::from("-");
        if t % control_interval == 0 && t > 0 {
            let new_target = controller.decide(&system_state, &metrics);

            let changed = new_target.num_replicas != target.num_replicas
                || (new_target.cpu_per_replica - target.cpu_per_replica).abs() > 0.01
                || (new_target.mem_per_replica - target.mem_per_replica).abs() > 0.01;

            if changed {
                action = format!(
                    "Scale → R:{} CPU:{:.1} Mem:{:.1}",
                    new_target.num_replicas, new_target.cpu_per_replica, new_target.mem_per_replica
                );
            }

            target = new_target;
        }

        // Store data point for CSV
        data_points.push((system_state.clone(), metrics.avg_latency_ms, action.clone()));

        // Print to console every 10 seconds (or less frequently for long runs)
        let print_interval = if duration > 3600 { 300 } else if duration > 600 { 60 } else { 10 };
        
        if output.console && t % print_interval == 0 {
            println!(
                "{:7} | {:5.0} | {:5} | {:8} | {:7.1} | {:7.1} | {:4.0} | {:4.0} | {:11.1} | {}",
                t,
                system_state.incoming_rps,
                system_state.waiting_requests,
                system_state.num_replicas,
                system_state.cpu_per_replica,
                system_state.mem_per_replica,
                system_state.cpu_util,
                system_state.mem_util,
                metrics.avg_latency_ms,
                action
            );
        }
    }

    // Write all data to CSV
    if let Some(ref mut file) = csv_file {
        for (state, latency, action) in data_points {
            writeln!(
                file,
                "{},{},{},{},{},{},{},{},{},{}",
                state.time_s,
                state.incoming_rps,
                state.waiting_requests,
                state.num_replicas,
                state.cpu_per_replica,
                state.mem_per_replica,
                state.cpu_util,
                state.mem_util,
                latency,
                action.replace(",", ";") // Avoid CSV issues
            )
            .expect("Failed to write CSV data");
        }
        println!("\n✓ CSV file written");
    }

    println!("\n✓ Scenario complete\n");
}

fn sanitize_filename(name: &str) -> String {
    name.to_lowercase()
        .replace(" ", "_")
        .replace("(", "")
        .replace(")", "")
        .replace("→", "to")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

// Short scenarios
pub fn scenario_steady_load(output: &OutputConfig) {
    run_scenario(
        "Steady Load (200 RPS)",
        WorkloadPattern::Steady { rps: 200.0 },
        120,
        output,
    );
}

pub fn scenario_step_increase(output: &OutputConfig) {
    run_scenario(
        "Step Increase (50 → 500 RPS at t=60)",
        WorkloadPattern::Step {
            base: 50.0,
            step_to: 500.0,
            step_at: 60,
        },
        180,
        output,
    );
}

pub fn scenario_burst(output: &OutputConfig) {
    run_scenario(
        "Traffic Burst (100 RPS → 800 RPS burst from t=30-90)",
        WorkloadPattern::Burst {
            base: 100.0,
            peak: 800.0,
            burst_start: 30,
            burst_end: 90,
        },
        150,
        output,
    );
}

// Long-running scenarios
pub fn scenario_gradual_growth(output: &OutputConfig) {
    run_scenario(
        "Gradual Traffic Growth (1 hour, 50 → 1000 RPS)",
        WorkloadPattern::Step {
            base: 50.0,
            step_to: 1000.0,
            step_at: 1800, // 30 minutes
        },
        3600, // 1 hour
        output,
    );
}

pub fn scenario_workday(output: &OutputConfig) {
    run_scenario(
        "Workday Traffic Pattern (8 hours)",
        WorkloadPattern::Burst {
            base: 100.0,
            peak: 500.0,
            burst_start: 3600,  // 1 hour in
            burst_end: 25200,   // 7 hours in (6 hour busy period)
        },
        28800, // 8 hours in seconds
        output,
    );
}

pub fn scenario_flash_sale(output: &OutputConfig) {
    run_scenario(
        "Flash Sale Event (30 minutes)",
        WorkloadPattern::Burst {
            base: 200.0,
            peak: 2000.0,
            burst_start: 300,  // 5 minutes in
            burst_end: 900,    // 15 minutes in (10 min sale)
        },
        1800, // 30 minutes
        output,
    );
}
