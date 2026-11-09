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

/// Aggregate live GPU presentation stages over the same one-second
/// reporting window as FPS. Durations are CPU wall-clock, not GPU timestamps.
#[cfg(feature = "gpu")]
#[derive(Debug, Default)]
pub struct GpuProfileWindow {
    total_ms: [f64; 4],
    frames: u64,
    reused: u64,
    total_bytes: u64,
}

#[cfg(feature = "gpu")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuProfileMeans {
    pub map_ms: f64,
    pub sprites_ms: f64,
    pub upload_ms: f64,
    pub present_ms: f64,
    pub reuse_percent: f64,
    pub uploaded_mib_per_frame: f64,
    pub frames: u64,
}

#[cfg(feature = "gpu")]
impl GpuProfileWindow {
    pub fn record(&mut self, profile: crate::gpu_renderer::GpuFrameProfile) {
        let times = [
            profile.map_build, profile.sprites_and_hud,
            profile.upload, profile.present,
        ];
        for (total, stage) in self.total_ms.iter_mut().zip(times) {
            *total += stage.as_secs_f64() * 1000.0;
        }
        self.frames += 1;
        self.reused += u64::from(profile.reused_map);
        self.total_bytes = self.total_bytes.saturating_add(
            u64::try_from(profile.uploaded_bytes).unwrap_or(u64::MAX),
        );
    }

    /// Consume a measurement interval without sorting or allocating.
    pub fn take_means(&mut self) -> Option<GpuProfileMeans> {
        if self.frames == 0 { return None; }
        let count = self.frames as f64;
        let result = GpuProfileMeans {
            map_ms: self.total_ms[0] / count,
            sprites_ms: self.total_ms[1] / count,
            upload_ms: self.total_ms[2] / count,
            present_ms: self.total_ms[3] / count,
            reuse_percent: self.reused as f64 * 100.0 / count,
            uploaded_mib_per_frame: self.total_bytes as f64 / (1024.0 * 1024.0 * count),
            frames: self.frames,
        };
        *self = Self::default();
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "gpu")]
    #[test]
    fn gpu_stage_profile_aggregates_mean_reuse_and_upload_without_leaking_state() {
        use crate::gpu_renderer::GpuFrameProfile;
        let mut stats = GpuProfileWindow::default();
        assert!(stats.take_means().is_none());
        stats.record(GpuFrameProfile {
            map_build: Duration::from_millis(4),
            sprites_and_hud: Duration::from_millis(2),
            upload: Duration::from_millis(1),
            present: Duration::from_millis(3),
            uploaded_bytes: 1_048_576,
            map_vertices: 500,
            reused_map: false,
        });
        stats.record(GpuFrameProfile {
            map_build: Duration::ZERO,
            sprites_and_hud: Duration::from_millis(4),
            upload: Duration::from_millis(3),
            present: Duration::from_millis(5),
            uploaded_bytes: 0,
            map_vertices: 500,
            reused_map: true,
        });
        let avg = stats.take_means().unwrap();
        assert_eq!(avg.frames, 2);
        assert_eq!(avg.map_ms, 2.0);
        assert_eq!(avg.sprites_ms, 3.0);
        assert_eq!(avg.upload_ms, 2.0);
        assert_eq!(avg.present_ms, 4.0);
        assert_eq!(avg.reuse_percent, 50.0);
        assert_eq!(avg.uploaded_mib_per_frame, 0.5);
        assert!(stats.take_means().is_none());
    }

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
