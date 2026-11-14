//! On-demand metering of the existing microphone capture stream.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;

const LEASE_MS: u64 = 500;

#[derive(Debug, Serialize)]
pub struct InputLevel {
    pub active: bool,
    /// Peak after Trix's microphone gain, normalized to 0..=1.
    pub peak: f32,
}

pub(super) struct InputLevelMeter {
    epoch: Instant,
    until_ms: AtomicU64,
    peak: AtomicU32,
    active: AtomicBool,
}

impl Default for InputLevelMeter {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            until_ms: AtomicU64::new(0),
            peak: AtomicU32::new(0),
            active: AtomicBool::new(false),
        }
    }
}

impl InputLevelMeter {
    pub fn set_active(&self, active: bool) {
        self.peak.store(0, Ordering::Relaxed);
        self.active.store(active, Ordering::Release);
    }

    pub fn sample(&self) -> InputLevel {
        self.sample_at(self.epoch.elapsed().as_millis() as u64)
    }

    fn sample_at(&self, now: u64) -> InputLevel {
        let previous = self.until_ms.swap(now + LEASE_MS, Ordering::Relaxed);
        let peak = self.peak.swap(0, Ordering::Relaxed);
        let active = self.active.load(Ordering::Acquire);
        InputLevel {
            active,
            peak: if active && previous > now { peak as f32 / 32768.0 } else { 0.0 },
        }
    }

    pub fn observe(&self, pcm: &[u8], gain: f32) {
        // With no settings reader, even querying the clock is skipped.
        if self.until_ms.load(Ordering::Relaxed) != 0 {
            self.observe_at(self.epoch.elapsed().as_millis() as u64, pcm, gain);
        }
    }

    fn observe_at(&self, now: u64, pcm: &[u8], gain: f32) {
        let deadline = self.until_ms.load(Ordering::Relaxed);
        if deadline == 0 {
            return;
        }
        if now >= deadline {
            // Do not cancel a lease another reader renewed meanwhile.
            let _ =
                self.until_ms.compare_exchange(deadline, 0, Ordering::Relaxed, Ordering::Relaxed);
            return;
        }
        let peak = pcm
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
            .max()
            .unwrap_or(0);
        let scaled = (f32::from(peak) * gain).round().clamp(0.0, 32768.0) as u32;
        self.peak.fetch_max(scaled, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn input_meter_only_measures_while_requested_and_expires() {
        let meter = InputLevelMeter::default();
        meter.set_active(true);
        meter.observe_at(10, &pcm(&[16000]), 1.0);
        assert_eq!(meter.sample_at(20).peak, 0.0);
        meter.observe_at(30, &pcm(&[16384]), 1.0);
        assert_eq!(meter.sample_at(40).peak, 0.5);
        meter.observe_at(600, &pcm(&[i16::MAX]), 1.0);
        assert_eq!(meter.until_ms.load(Ordering::Relaxed), 0);
        assert_eq!(meter.sample_at(610).peak, 0.0);
    }

    #[test]
    fn input_meter_uses_both_channels_and_resets_each_read() {
        let meter = InputLevelMeter::default();
        meter.set_active(true);
        meter.sample_at(1);
        meter.observe_at(2, &pcm(&[0, -16384, 1000, 0]), 1.0);
        meter.observe_at(3, &pcm(&[0, 0]), 1.0);
        let level = meter.sample_at(4);
        assert!(level.active);
        assert_eq!(level.peak, 0.5);
        assert_eq!(meter.sample_at(5).peak, 0.0);
    }

    #[test]
    fn input_meter_handles_negative_full_scale_gain_and_clipping() {
        let meter = InputLevelMeter::default();
        meter.set_active(true);
        meter.sample_at(1);
        meter.observe_at(2, &pcm(&[i16::MIN]), 1.0);
        assert_eq!(meter.sample_at(3).peak, 1.0);
        meter.observe_at(4, &pcm(&[-16384]), 0.25);
        assert_eq!(meter.sample_at(5).peak, 0.125);
        meter.observe_at(6, &pcm(&[30000]), 2.0);
        assert_eq!(meter.sample_at(7).peak, 1.0);
        meter.observe_at(8, &pcm(&[30000]), 0.0);
        assert_eq!(meter.sample_at(9).peak, 0.0);
    }

    #[test]
    fn input_meter_clears_on_stop_and_does_not_replay_old_peaks() {
        let meter = InputLevelMeter::default();
        meter.set_active(true);
        meter.sample_at(1);
        meter.observe_at(2, &pcm(&[i16::MIN]), 1.0);
        meter.set_active(false);
        let stopped = meter.sample_at(3);
        assert!(!stopped.active);
        assert_eq!(stopped.peak, 0.0);
        meter.set_active(true);
        meter.observe_at(4, &pcm(&[16000]), 1.0);
        assert_eq!(meter.sample_at(1000).peak, 0.0);
    }
}
