mod types;
mod workload;
mod simulator;
mod metrics;
mod controller;
mod scenarios;

use clap::{Parser, Subcommand};
use scenarios::*;

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
    /// Run all scenarios
    All,
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
        None => {
            // Default: run step scenario
            scenario_step_increase(&output);
        }
    }
}

