/// Generates incoming request rate (RPS) as a function of time

#[derive(Debug, Clone)]
pub enum WorkloadPattern {
    Steady { rps: f64 },
    Step { base: f64, step_to: f64, step_at: u32 },
    Burst { base: f64, peak: f64, burst_start: u32, burst_end: u32 },
}

impl WorkloadPattern {
    pub fn rps_at(&self, t: u32) -> f64 {
        match self {
            WorkloadPattern::Steady { rps } => *rps,
            WorkloadPattern::Step { base, step_to, step_at } => {
                if t >= *step_at {
                    *step_to
                } else {
                    *base
                }
            }
            WorkloadPattern::Burst { base, peak, burst_start, burst_end } => {
                if t >= *burst_start && t < *burst_end {
                    *peak
                } else {
                    *base
                }
            }
        }
    }
}
