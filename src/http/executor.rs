use anyhow::Result;
use std::process::{Child, Command, Stdio};

/// Represents a running HTTP worker instance (toy service) on a given port.
pub struct Worker {
    pub port: u16,
    child: Child,
}

pub struct Executor {
    workers: Vec<Worker>,
    base_port: u16,
    cpu_factor: f64,
    mem_factor: f64,
}

impl Executor {
    pub fn new(base_port: u16, cpu_factor: f64, mem_factor: f64) -> Self {
        Self {
            workers: Vec::new(),
            base_port,
            cpu_factor,
            mem_factor,
        }
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub fn worker_urls(&self) -> Vec<String> {
        self.workers
            .iter()
            .map(|w| format!("http://127.0.0.1:{}", w.port))
            .collect()
    }

    /// Apply a new target shape (replicas + vertical factors) globally to all workers.
    pub fn apply_target(&mut self, target_replicas: u32, cpu_factor: f64, mem_factor: f64) -> Result<()> {
        let target = target_replicas as usize;
        let v_changed = (cpu_factor - self.cpu_factor).abs() > 0.01 || (mem_factor - self.mem_factor).abs() > 0.01;

        if v_changed {
            // Restart all workers with new vertical shape
            while let Some(mut w) = self.workers.pop() {
                println!("[executor] restarting worker on port {} for new v-shape", w.port);
                let _ = w.child.kill();
                let _ = w.child.wait();
            }
            self.cpu_factor = cpu_factor;
            self.mem_factor = mem_factor;
        }

        self.ensure_workers(target)
    }

    /// Ensure there are exactly `target` workers running (scale up/down).
    pub fn ensure_workers(&mut self, target: usize) -> Result<()> {
        // Scale up
        while self.workers.len() < target {
            let port = self.base_port + self.workers.len() as u16;
            let worker = spawn_worker(port, self.cpu_factor, self.mem_factor)?;
            println!("[executor] started worker on port {}", port);
            self.workers.push(worker);
        }

        // Scale down
        while self.workers.len() > target {
            if let Some(mut w) = self.workers.pop() {
                println!("[executor] stopping worker on port {}", w.port);
                let _ = w.child.kill();
                let _ = w.child.wait();
            }
        }

        Ok(())
    }
}

fn spawn_worker(port: u16, cpu_factor: f64, mem_factor: f64) -> Result<Worker> {
    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .arg("service")
        .arg("--port")
        .arg(port.to_string())
        .arg("--cpu-factor")
        .arg(format!("{:.3}", cpu_factor))
        .arg("--mem-factor")
        .arg(format!("{:.3}", mem_factor))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    Ok(Worker { port, child })
}
