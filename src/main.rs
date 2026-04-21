mod shared;
mod sim;
mod http;

use clap::{Parser, Subcommand};
use sim::scenarios::*;
use http::service::run_service;
use http::loadgen::{
    steady as loadgen_steady, step as loadgen_step, burst as loadgen_burst,
    ramp as loadgen_ramp, sawtooth as loadgen_sawtooth, wave as loadgen_wave,
    double_burst as loadgen_double_burst,
};

#[derive(Parser)]
#[command(name = "autoscaler")]
#[command(about = "Unified autoscaler simulation", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Write output to CSV files in data/ directory
    #[arg(long, global = true)]
    csv: bool,

    /// Suppress console output (useful with --csv)
    #[arg(long, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a specific scenario
    Scenario {
        /// Scenario name: steady, step, burst, growth, workday, flash-sale
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Run all simulation scenarios
    All,
    /// Run the real HTTP toy service
    Service {
        /// Port to listen on (default 8080)
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// CPU factor per request — controls how much CPU work each request does (1.0 = baseline)
        #[arg(long, default_value_t = 1.0)]
        cpu_factor: f64,
        /// Memory factor per request — controls allocation size per request (1.0 = baseline)
        #[arg(long, default_value_t = 1.0)]
        mem_factor: f64,
        /// Max requests processed concurrently — the vertical scaling knob
        #[arg(long, default_value_t = 32)]
        max_concurrency: usize,
        /// Number of threads in the CPU work pool — limits real CPU parallelism.
        /// Set to ceil(cpu_per_replica); cgroup cpu.max enforces the hard OS limit on top.
        #[arg(long, default_value_t = 2)]
        worker_threads: usize,
    },
    /// Run HTTP controller + proxy against local workers
    HttpControl {
        /// Base port for worker instances (e.g. 9000 → 9000, 9001, …)
        #[arg(long, default_value_t = 9000)]
        base_port: u16,
        /// Stable proxy port — point your load generator here
        #[arg(long, default_value_t = 8080)]
        proxy_port: u16,
        /// Initial number of worker instances
        #[arg(long, default_value_t = 1)]
        initial_replicas: u32,
        /// Duration in seconds to run the controller
        #[arg(long, default_value_t = 120)]
        duration: u32,
        /// Control interval in seconds
        #[arg(long, default_value_t = 10)]
        control_interval: u32,
        /// CPU factor per request for workers (sets workload type, fixed for the run)
        #[arg(long, default_value_t = 1.0)]
        cpu_factor: f64,
        /// Memory factor per request for workers (sets workload type, fixed for the run)
        #[arg(long, default_value_t = 1.0)]
        mem_factor: f64,
    },
    /// Run HTTP load generator against the toy service
    Loadgen {
        /// Base URL(s) of the service, comma-separated (e.g. http://127.0.0.1:8080,http://127.0.0.1:8081)
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        base_url: String,
        /// Endpoint: cpu-heavy | mem-heavy | mixed
        #[arg(long, default_value = "cpu-heavy")]
        endpoint: String,
        /// Pattern: steady | step | burst | ramp | sawtooth | wave | double-burst
        #[arg(long, default_value = "steady")]
        pattern: String,
        /// Duration in seconds
        #[arg(long, default_value_t = 180)]
        duration: u32,
        /// Base RPS — idle baseline for all patterns
        #[arg(long, default_value_t = 5.0)]
        base_rps: f64,
        /// Peak RPS — ceiling for burst/ramp/sawtooth/wave/double-burst
        #[arg(long, default_value_t = 100.0)]
        peak_rps: f64,
        /// Step-to RPS (step only)
        #[arg(long, default_value_t = 80.0)]
        step_to: f64,
        /// Time in seconds at which step happens (step only)
        #[arg(long, default_value_t = 30)]
        step_at: u32,
        /// Burst/spike start time in seconds (burst, double-burst)
        #[arg(long, default_value_t = 20)]
        burst_start: u32,
        /// Burst/spike end time in seconds (burst, double-burst)
        #[arg(long, default_value_t = 60)]
        burst_end: u32,
        /// Second burst start time (double-burst only)
        #[arg(long, default_value_t = 100)]
        burst2_start: u32,
        /// Second burst end time (double-burst only)
        #[arg(long, default_value_t = 140)]
        burst2_end: u32,
        /// How many seconds the ramp takes to reach peak (ramp only)
        #[arg(long, default_value_t = 60)]
        ramp_duration: u32,
        /// Sawtooth/wave period in seconds
        #[arg(long, default_value_t = 40)]
        period: u32,
        /// Wave centre RPS (wave only; use base_rps as centre, peak_rps sets amplitude)
        #[arg(long, default_value_t = 40.0)]
        wave_center: f64,
    },
}

fn main() {
    let cli = Cli::parse();

    let output = OutputConfig {
        console: !cli.quiet,
        csv: cli.csv,
        csv_dir: String::from("data"),
    };

    match &cli.command {
        Some(Commands::Scenario { name }) => match name.as_str() {
            "steady" => scenario_steady_load(&output),
            "step" => scenario_step_increase(&output),
            "burst" => scenario_burst(&output),
            "growth" => scenario_gradual_growth(&output),
            "workday" => scenario_workday(&output),
            "flash-sale" => scenario_flash_sale(&output),
            _ => {
                eprintln!("Unknown scenario: {}", name);
                eprintln!("Available: steady, step, burst, growth, workday, flash-sale");
                std::process::exit(1);
            }
        },
        Some(Commands::All) => {
            scenario_steady_load(&output);
            scenario_step_increase(&output);
            scenario_burst(&output);
            scenario_gradual_growth(&output);
            scenario_workday(&output);
            scenario_flash_sale(&output);
        }
        Some(Commands::Service { port, cpu_factor, mem_factor, max_concurrency, worker_threads }) => {
            if let Err(e) = run_service(*port, *cpu_factor, *mem_factor, *max_concurrency, *worker_threads) {
                eprintln!("Service error: {:#}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::HttpControl {
            base_port,
            proxy_port,
            initial_replicas,
            duration,
            control_interval,
            cpu_factor,
            mem_factor,
        }) => {
            let csv_dir = if cli.csv { Some(output.csv_dir.as_str()) } else { None };
            if let Err(e) = http::controller::run_http_controller(
                *base_port, *proxy_port, *initial_replicas, *duration, *control_interval,
                *cpu_factor, *mem_factor, csv_dir,
            ) {
                eprintln!("HttpControl error: {:#}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::Loadgen {
            base_url, endpoint, pattern, duration,
            base_rps, peak_rps,
            step_to, step_at,
            burst_start, burst_end,
            burst2_start, burst2_end,
            ramp_duration, period, wave_center,
        }) => {
            let res = match pattern.as_str() {
                "steady" => loadgen_steady(base_url, endpoint, *base_rps, *duration),
                "step"   => loadgen_step(base_url, endpoint, *base_rps, *step_to, *step_at, *duration),
                "burst"  => loadgen_burst(base_url, endpoint, *base_rps, *peak_rps, *burst_start, *burst_end, *duration),
                "ramp"   => loadgen_ramp(base_url, endpoint, *base_rps, *peak_rps, *ramp_duration, *duration),
                "sawtooth" => loadgen_sawtooth(base_url, endpoint, *base_rps, *peak_rps, *period, *duration),
                "wave"   => loadgen_wave(base_url, endpoint, *wave_center, *peak_rps, *period, *duration),
                "double-burst" => loadgen_double_burst(
                    base_url, endpoint,
                    *base_rps, *peak_rps,
                    *burst_start, *burst_end,
                    *burst2_start, *burst2_end,
                    *duration,
                ),
                _ => {
                    eprintln!("Unknown pattern: {} (expected steady|step|burst|ramp|sawtooth|wave|double-burst)", pattern);
                    std::process::exit(1);
                }
            };

            if let Err(e) = res {
                eprintln!("Loadgen error: {:#}", e);
                std::process::exit(1);
            }
        }
        None => {
            // Default: run step scenario
            scenario_step_increase(&output);
        }
    }
}

