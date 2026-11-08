//! Bounded rendering latency sample, for comparing CPU and GPU backends
//! on the same system. Measures render + surface presentation, not network
//! processing or minifb's pacing sleep.

use std::time::Duration;

const MAX_SAMPLES: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetrics {
    pub mean_ms: f32,
    pub p50_ms: f32,
    pub p95_ms: f32,
    pub p99_ms: f32,
    pub count: usize,
}

#[derive(Debug, Default)]
pub struct FrameTiming {
    samples: Vec<f32>,
}

impl FrameTiming {
    pub fn record(&mut self, duration: Duration) {
        // Never feed NaN or +/- infinity into percentiles.
        let ms = duration.as_secs_f64() * 1000.0;
        if ms.is_finite() {
            if self.samples.len() == MAX_SAMPLES {
                self.samples.remove(0);
            }
            self.samples.push(ms.min(f32::MAX as f64) as f32);
        }
    }

    /// Nearest-rank quantiles: indices ceil(N * percentile) - 1.
    /// Consumes the collected window and resets for the next title interval.
    pub fn take_metrics(&mut self) -> Option<FrameMetrics> {
        if self.samples.is_empty() { return None; }
        self.samples.sort_by(f32::total_cmp);
        let len = self.samples.len();
        let rank = |pct: usize| (len * pct).div_ceil(100).saturating_sub(1);
        let mean_ms = self.samples.iter().map(|&value| value as f64)
            .sum::<f64>() as f32 / len as f32;
        let result = FrameMetrics {
            mean_ms,
            p50_ms: self.samples[rank(50)],
            p95_ms: self.samples[rank(95)],
            p99_ms: self.samples[rank(99)],
            count: len,
        };
        self.samples.clear();
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_expose_rare_long_render_frames() {
        let mut timings = FrameTiming::default();
        for _ in 0..95 { timings.record(Duration::from_millis(5)); }
        for _ in 0..5 { timings.record(Duration::from_millis(50)); }
        let p = timings.take_metrics().unwrap();
        assert_eq!(p.count, 100);
        assert_eq!(p.p50_ms, 5.0);
        assert_eq!(p.p95_ms, 5.0);
        assert_eq!(p.p99_ms, 50.0);
        assert!((p.mean_ms - 7.25).abs() < 0.01);
        assert_eq!(timings.take_metrics(), None);
    }

    #[test]
    fn small_samples_have_valid_ranks_and_reset() {
        let mut s = FrameTiming::default();
        s.record(Duration::from_micros(500));
        let p = s.take_metrics().unwrap();
        assert_eq!(p.p50_ms, 0.5);
        assert_eq!(p.p95_ms, 0.5);
        assert_eq!(p.p99_ms, 0.5);
        s.record(Duration::from_millis(22));
        assert_eq!(s.take_metrics().unwrap().p95_ms, 22.0);
    }

    #[test]
    fn sample_limit_and_outliers_do_not_overflow() {
        let mut s = FrameTiming::default();
        for _ in 0..(MAX_SAMPLES + 10) {
            s.record(Duration::from_micros(100));
        }
        assert_eq!(s.take_metrics().unwrap().count, MAX_SAMPLES);
        assert!(s.take_metrics().is_none());
    }
}
