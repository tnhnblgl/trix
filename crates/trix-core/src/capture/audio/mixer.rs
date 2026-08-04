//! Mixing two capture sources into the one PCM stream clips carry.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

/// Converts a 0–100 level into a linear multiplier: the percentage squared.
///
/// Scaling amplitude directly by the percentage makes a slider feel dead —
/// loudness is roughly logarithmic in amplitude, so every audible change
/// crowds into the bottom third of the travel. Squaring is the standard fader
/// taper. Values above 100 clamp: 100 is unity and the maximum, so Trix can
/// never be the reason a clip clips.
pub fn percent_to_gain(percent: u32) -> f32 {
    let fraction = percent.min(100) as f32 / 100.0;
    fraction * fraction
}

/// Scales interleaved i16 PCM in place.
///
/// Unity returns untouched rather than multiplying by 1.0, which keeps the
/// default single-source path bit-for-bit identical to what Trix recorded
/// before mixing existed.
pub fn apply_gain(pcm: &mut [u8], gain: f32) {
    if gain == 1.0 {
        return;
    }
    for sample in pcm.chunks_exact_mut(2) {
        let value = f32::from(i16::from_le_bytes([sample[0], sample[1]]));
        let scaled = (value * gain).round().clamp(f32::from(i16::MIN), f32::from(i16::MAX));
        sample.copy_from_slice(&(scaled as i16).to_le_bytes());
    }
}

/// Adds `src`, scaled by `gain`, into `dst` sample-wise, clamping to i16.
///
/// Mixes `min(dst.len(), src.len())` whole samples and leaves any excess of
/// `dst` untouched: a source that is momentarily short must not shift or
/// truncate the other, which would be a permanent A/V desync rather than a
/// glitch. Summing in i32 and clamping means a loud game plus a loud voice
/// distorts; wrapping would invert the waveform and produce a scream.
pub fn mix_into(dst: &mut [u8], src: &[u8], gain: f32) {
    let common = dst.len().min(src.len());
    let whole_samples = common - (common % 2);
    let pairs = dst[..whole_samples].chunks_exact_mut(2).zip(src[..whole_samples].chunks_exact(2));
    for (into, from) in pairs {
        let a = i32::from(i16::from_le_bytes([into[0], into[1]]));
        let b = (f32::from(i16::from_le_bytes([from[0], from[1]])) * gain).round() as i32;
        let sum = (a + b).clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        into.copy_from_slice(&(sum as i16).to_le_bytes());
    }
}

/// The two capture levels, as whole percentages.
///
/// The daemon owns exactly one of these for its whole lifetime and hands
/// clones of the `Arc` to each capture session, so "the levels the daemon
/// holds" and "the levels the session reads" are the same thing whether or
/// not anything is armed. Read once per pump, written by `config.set`:
/// `Relaxed` is right because a level arriving one pump later than it could
/// have is inaudible, and no other state is ordered against it.
pub struct AudioGains {
    system: AtomicU32,
    mic: AtomicU32,
}

impl AudioGains {
    pub fn new(system_percent: u32, mic_percent: u32) -> Arc<Self> {
        Arc::new(Self {
            system: AtomicU32::new(system_percent.min(100)),
            mic: AtomicU32::new(mic_percent.min(100)),
        })
    }

    /// Applied to audio captured from here on. Audio already in the replay
    /// ring keeps the levels it was captured at.
    pub fn set(&self, system_percent: u32, mic_percent: u32) {
        self.system.store(system_percent.min(100), Ordering::Relaxed);
        self.mic.store(mic_percent.min(100), Ordering::Relaxed);
    }

    pub fn system_percent(&self) -> u32 {
        self.system.load(Ordering::Relaxed)
    }

    pub fn mic_percent(&self) -> u32 {
        self.mic.load(Ordering::Relaxed)
    }

    pub fn system(&self) -> f32 {
        percent_to_gain(self.system_percent())
    }

    pub fn mic(&self) -> f32 {
        percent_to_gain(self.mic_percent())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a little-endian i16 PCM buffer from sample values.
    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    /// Reads a little-endian i16 PCM buffer back into sample values.
    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    }

    #[test]
    fn full_volume_is_unity() {
        assert_eq!(percent_to_gain(100), 1.0);
    }

    #[test]
    fn zero_volume_is_silence() {
        assert_eq!(percent_to_gain(0), 0.0);
    }

    #[test]
    fn the_taper_is_the_percentage_squared() {
        // Half travel is a quarter of the amplitude, which is roughly half
        // the perceived loudness. A linear taper would put every audible
        // change in the bottom third of the slider.
        assert!((percent_to_gain(50) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn the_taper_never_amplifies() {
        // 100 is the maximum and unity: Trix is never the reason a clip clips.
        for percent in 0..=200u32 {
            assert!(percent_to_gain(percent) <= 1.0, "gain exceeded unity at {percent}");
        }
    }

    #[test]
    fn unity_gain_leaves_the_buffer_byte_for_byte_identical() {
        // The single-source default path runs through apply_gain. If unity
        // were not free, every clip anyone records today would change.
        let original = pcm(&[1000, -1000, 32767, -32768, 0]);
        let mut buffer = original.clone();
        apply_gain(&mut buffer, 1.0);
        assert_eq!(buffer, original);
    }

    #[test]
    fn zero_gain_silences_the_buffer() {
        let mut buffer = pcm(&[1000, -1000, 32767]);
        apply_gain(&mut buffer, 0.0);
        assert_eq!(samples(&buffer), vec![0, 0, 0]);
    }

    #[test]
    fn gain_scales_samples_not_bytes() {
        // Treating the buffer as u8 would corrupt every sample while still
        // producing a plausible-looking byte count.
        let mut buffer = pcm(&[1000, -2000]);
        apply_gain(&mut buffer, 0.5);
        assert_eq!(samples(&buffer), vec![500, -1000]);
    }

    #[test]
    fn mixing_sums_sample_wise() {
        let mut dst = pcm(&[100, 200, -300]);
        mix_into(&mut dst, &pcm(&[10, 20, 30]), 1.0);
        assert_eq!(samples(&dst), vec![110, 220, -270]);
    }

    #[test]
    fn mixing_applies_the_source_gain() {
        let mut dst = pcm(&[100]);
        mix_into(&mut dst, &pcm(&[200]), 0.5);
        assert_eq!(samples(&dst), vec![200]);
    }

    #[test]
    fn mixing_clamps_positive_rather_than_wrapping() {
        // Wrapping would invert the waveform and produce a scream. Clamping
        // distorts, which is merely unpleasant.
        let mut dst = pcm(&[30000, 20000]);
        mix_into(&mut dst, &pcm(&[30000, 20000]), 1.0);
        assert_eq!(samples(&dst), vec![i16::MAX, i16::MAX]);
    }

    #[test]
    fn mixing_clamps_negative_rather_than_wrapping() {
        let mut dst = pcm(&[-30000]);
        mix_into(&mut dst, &pcm(&[-30000]), 1.0);
        assert_eq!(samples(&dst), vec![i16::MIN]);
    }

    #[test]
    fn mixing_a_short_source_leaves_the_rest_of_the_destination_alone() {
        // One source running momentarily short must not shift or truncate
        // the other — that is a permanent A/V desync, not a glitch.
        let mut dst = pcm(&[100, 200, 300, 400]);
        mix_into(&mut dst, &pcm(&[10, 20]), 1.0);
        assert_eq!(samples(&dst), vec![110, 220, 300, 400]);
    }

    #[test]
    fn gains_round_trip_through_the_shared_cell() {
        let gains = AudioGains::new(100, 100);
        assert_eq!(gains.system_percent(), 100);
        assert_eq!(gains.mic_percent(), 100);
        gains.set(40, 0);
        assert_eq!(gains.system_percent(), 40);
        assert_eq!(gains.mic_percent(), 0);
        assert_eq!(gains.mic(), 0.0);
    }

    #[test]
    fn gains_clamp_a_value_above_the_bound() {
        // The daemon rejects these before they arrive, but AudioGains is
        // public API and must not produce an amplifying gain for anyone.
        let gains = AudioGains::new(400, 400);
        assert_eq!(gains.system(), 1.0);
        assert_eq!(gains.mic(), 1.0);
    }
}
