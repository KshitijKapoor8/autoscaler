/// Generates incoming request rate (RPS) as a function of time

#[derive(Debug, Clone)]
pub enum WorkloadPattern {
    Steady { rps: f64 },
    Step { base: f64, step_to: f64, step_at: u32 },
    Burst { base: f64, peak: f64, burst_start: u32, burst_end: u32 },
    /// Linear ramp from `base` up to `peak` over `ramp_duration` seconds, then holds at peak.
    Ramp { base: f64, peak: f64, ramp_duration: u32 },
    /// Repeating sawtooth: linear ramp from `base` to `peak` over `period` seconds, then
    /// instant drop back to `base`. Good for testing oscillation and recovery.
    Sawtooth { base: f64, peak: f64, period: u32 },
    /// Sinusoidal oscillation around `center` with the given `amplitude` and `period_s`.
    /// RPS is clamped to >= 0.
    Wave { center: f64, amplitude: f64, period_s: u32 },
    /// Two independent bursts. Tests recovery between events.
    DoubleBurst {
        base: f64,
        peak: f64,
        burst1_start: u32,
        burst1_end: u32,
        burst2_start: u32,
        burst2_end: u32,
    },
}

impl WorkloadPattern {
    pub fn rps_at(&self, t: u32) -> f64 {
        match self {
            WorkloadPattern::Steady { rps } => *rps,

            WorkloadPattern::Step { base, step_to, step_at } => {
                if t >= *step_at { *step_to } else { *base }
            }

            WorkloadPattern::Burst { base, peak, burst_start, burst_end } => {
                if t >= *burst_start && t < *burst_end { *peak } else { *base }
            }

            WorkloadPattern::Ramp { base, peak, ramp_duration } => {
                if *ramp_duration == 0 || t >= *ramp_duration {
                    *peak
                } else {
                    base + (peak - base) * (t as f64 / *ramp_duration as f64)
                }
            }

            WorkloadPattern::Sawtooth { base, peak, period } => {
                if *period == 0 { return *base; }
                let phase = t % period;
                base + (peak - base) * (phase as f64 / *period as f64)
            }

            WorkloadPattern::Wave { center, amplitude, period_s } => {
                if *period_s == 0 { return *center; }
                let theta = 2.0 * std::f64::consts::PI * t as f64 / *period_s as f64;
                (center + amplitude * theta.sin()).max(0.0)
            }

            WorkloadPattern::DoubleBurst {
                base, peak,
                burst1_start, burst1_end,
                burst2_start, burst2_end,
            } => {
                if (t >= *burst1_start && t < *burst1_end)
                    || (t >= *burst2_start && t < *burst2_end)
                {
                    *peak
                } else {
                    *base
                }
            }
        }
    }
}
