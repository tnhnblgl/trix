//! Mixing two capture sources into the one PCM stream clips carry.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::sync::mpsc::Receiver;

use anyhow::Result;

use super::{
    AudioPacket, AudioTimeline, ENCODER_BLOCK_ALIGN,
    source::{AudioCapture, AudioSourceKind},
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

/// One capture source: its channel, its timeline, and the PCM it has emitted
/// but that has not yet been matched against the other source.
struct Source {
    kind: AudioSourceKind,
    rx: Receiver<AudioPacket>,
    timeline: AudioTimeline,
    staged: Vec<u8>,
    capture: Option<AudioCapture>,
}

/// Sums the open capture sources into the single PCM stream clips carry.
///
/// Both timelines anchor to the same instant — the first video frame's QPC
/// stamp — and both are pumped to the same target, silence-filling whatever
/// their device did not supply. That makes them frame-aligned by construction,
/// so mixing is addition and needs no resampling or drift correction.
///
/// The surface mirrors [`AudioTimeline`]'s deliberately: `replay` and `record`
/// swap one for the other and their call sites lose an argument. It owns the
/// capture threads too, so each consumer has exactly one thing to hold and one
/// thing to stop rather than a handle and a receiver per source.
pub struct AudioMixer {
    system: Option<Source>,
    mic: Option<Source>,
    gains: Arc<AudioGains>,
    frames_emitted: u64,
}

impl AudioMixer {
    /// Opens whichever sources have a non-zero level.
    ///
    /// A level of 0 leaves that stream unopened rather than opened and
    /// multiplied by zero: Windows shows a microphone indicator whenever a
    /// process holds an input stream, and a recorder holding the microphone
    /// open while its own slider reads 0 is indistinguishable from one that is
    /// lying about it.
    ///
    /// Never fails. A source that cannot be opened — no microphone plugged in,
    /// device held exclusively, driver refusal — is logged and omitted, so
    /// audio can never be the reason an arm fails.
    pub fn start_sources(gains: Arc<AudioGains>) -> Self {
        let system = (gains.system_percent() > 0)
            .then(|| Self::open(AudioSourceKind::SystemAudio))
            .flatten();
        let mic =
            (gains.mic_percent() > 0).then(|| Self::open(AudioSourceKind::Microphone)).flatten();
        Self { system, mic, gains, frames_emitted: 0 }
    }

    fn open(kind: AudioSourceKind) -> Option<Source> {
        match AudioCapture::start(kind) {
            Ok((capture, rx)) => Some(Source {
                kind,
                rx,
                timeline: AudioTimeline::new(),
                staged: Vec::new(),
                capture: Some(capture),
            }),
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "{} unavailable, recording continues without it",
                    kind.label()
                );
                None
            }
        }
    }

    /// The same mixer over receivers the caller supplies. Without this seam
    /// every mixing test would need a real microphone attached to the machine.
    pub fn with_sources(
        system: Option<Receiver<AudioPacket>>,
        mic: Option<Receiver<AudioPacket>>,
        gains: Arc<AudioGains>,
    ) -> Self {
        let build = |kind: AudioSourceKind, rx: Receiver<AudioPacket>| Source {
            kind,
            rx,
            timeline: AudioTimeline::new(),
            staged: Vec::new(),
            capture: None,
        };
        Self {
            system: system.map(|rx| build(AudioSourceKind::SystemAudio, rx)),
            mic: mic.map(|rx| build(AudioSourceKind::Microphone, rx)),
            gains,
            frames_emitted: 0,
        }
    }

    /// True when at least one source opened. This is `RecorderSettings::with_audio`:
    /// with both levels at 0 the MP4 carries no audio stream at all rather
    /// than a track of silence.
    pub fn active(&self) -> bool {
        self.system.is_some() || self.mic.is_some()
    }

    /// Anchors every open timeline; the first call wins.
    pub fn start(&mut self, t0_qpc: i64) {
        for source in [self.system.as_mut(), self.mic.as_mut()].into_iter().flatten() {
            source.timeline.start(t0_qpc);
        }
    }

    pub fn started(&self) -> bool {
        [self.system.as_ref(), self.mic.as_ref()]
            .into_iter()
            .flatten()
            .any(|source| source.timeline.started())
    }

    pub fn frames_emitted(&self) -> u64 {
        self.frames_emitted
    }

    /// Frames for which *every* open source emitted synthesized silence.
    ///
    /// The minimum rather than the sum: a frame the microphone filled with
    /// silence while the game was loud is not a silent frame in the mix.
    pub fn silence_frames_emitted(&self) -> u64 {
        [self.system.as_ref(), self.mic.as_ref()]
            .into_iter()
            .flatten()
            .map(|source| source.timeline.silence_frames_emitted())
            .min()
            .unwrap_or(0)
    }

    /// Drains one source's timeline into its staging buffer.
    ///
    /// The timeline's `start_frame` is dropped deliberately: a timeline emits
    /// one gapless consecutive stream, so appending preserves order, and the
    /// mixer's own counter is the frame authority for the mixed stream.
    fn stage(source: Option<&mut Source>, target_qpc: i64) {
        let Some(Source { rx, timeline, staged, .. }) = source else { return };
        timeline.pump(rx, target_qpc, &mut |_start_frame, pcm| staged.extend_from_slice(pcm));
    }

    /// Emits gapless consecutive mixed chunks as `(start_frame, pcm)`.
    pub fn pump(&mut self, target_qpc: i64, sink: &mut impl FnMut(u64, &[u8])) {
        Self::stage(self.system.as_mut(), target_qpc);
        Self::stage(self.mic.as_mut(), target_qpc);

        // Only the span both sources have supplied can be mixed. Holding the
        // excess back costs one pump of latency; emitting it would put audio
        // on the timeline that can never be corrected.
        let available = match (&self.system, &self.mic) {
            (Some(system), Some(mic)) => system.staged.len().min(mic.staged.len()),
            (Some(only), None) | (None, Some(only)) => only.staged.len(),
            (None, None) => return,
        };
        let bytes = available - (available % ENCODER_BLOCK_ALIGN);
        if bytes == 0 {
            return;
        }

        let mixed = match (&mut self.system, &mut self.mic) {
            (Some(system), Some(mic)) => {
                let mut base: Vec<u8> = system.staged.drain(..bytes).collect();
                apply_gain(&mut base, self.gains.system());
                let overlay: Vec<u8> = mic.staged.drain(..bytes).collect();
                mix_into(&mut base, &overlay, self.gains.mic());
                base
            }
            (Some(system), None) => {
                let mut base: Vec<u8> = system.staged.drain(..bytes).collect();
                apply_gain(&mut base, self.gains.system());
                base
            }
            (None, Some(mic)) => {
                let mut base: Vec<u8> = mic.staged.drain(..bytes).collect();
                apply_gain(&mut base, self.gains.mic());
                base
            }
            (None, None) => return,
        };

        sink(self.frames_emitted, &mixed);
        self.frames_emitted += (bytes / ENCODER_BLOCK_ALIGN) as u64;
    }

    pub fn log_diagnostics(&self) {
        for source in [self.system.as_ref(), self.mic.as_ref()].into_iter().flatten() {
            source.timeline.log_diagnostics(source.kind.label());
        }
    }

    /// Stops every capture thread, reporting the first failure.
    ///
    /// `&mut self` rather than `self`: both consumers hold the mixer as a
    /// field of a struct they only have a `&mut` to when they shut down, and
    /// taking it by value would force each of them to swap in a throwaway
    /// mixer just to get an owned one. Idempotent — each handle is taken, so
    /// a second call stops nothing and succeeds.
    pub fn stop(&mut self) -> Result<()> {
        let mut outcome = Ok(());
        for source in [self.system.as_mut(), self.mic.as_mut()].into_iter().flatten() {
            let Some(capture) = source.capture.take() else { continue };
            if let Err(e) = capture.stop() {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "{} capture did not stop cleanly",
                    source.kind.label()
                );
                if outcome.is_ok() {
                    outcome = Err(e);
                }
            }
        }
        outcome
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

    use crate::capture::audio::{AudioPacket, ENCODER_BLOCK_ALIGN, SAMPLE_RATE, frames_to_100ns};
    use std::sync::mpsc::{Receiver, Sender, channel};

    /// A source channel pre-loaded with one packet of `value` in every sample,
    /// starting at the timeline origin.
    fn source(value: i16, frames: usize) -> (Sender<AudioPacket>, Receiver<AudioPacket>) {
        let (tx, rx) = channel();
        let samples = vec![value; frames * ENCODER_BLOCK_ALIGN / 2];
        tx.send(AudioPacket { qpc_100ns: 0, data: pcm(&samples) }).expect("receiver is alive");
        (tx, rx)
    }

    /// Collects everything a mixer emits in one pump into (start_frame, samples).
    fn drain(mixer: &mut AudioMixer, target_qpc: i64) -> Vec<(u64, Vec<i16>)> {
        let mut out = Vec::new();
        mixer.pump(target_qpc, &mut |start_frame, chunk| out.push((start_frame, samples(chunk))));
        out
    }

    #[test]
    fn a_mixer_with_no_sources_is_inactive_and_emits_nothing() {
        let mut mixer = AudioMixer::with_sources(None, None, AudioGains::new(100, 100));
        assert!(!mixer.active());
        mixer.start(0);
        assert!(drain(&mut mixer, frames_to_100ns(480)).is_empty());
    }

    #[test]
    fn one_source_passes_through_scaled() {
        let (_tx, rx) = source(1000, 480);
        let mut mixer = AudioMixer::with_sources(Some(rx), None, AudioGains::new(50, 100));
        assert!(mixer.active());
        mixer.start(0);
        let emitted = drain(&mut mixer, frames_to_100ns(480));
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].0, 0, "the first chunk starts at frame 0");
        // 50% is a gain of 0.25 (the squared taper).
        assert!(emitted[0].1.iter().all(|&s| s == 250), "unexpected samples");
    }

    #[test]
    fn two_sources_are_summed_frame_aligned() {
        let (_a, system) = source(1000, 480);
        let (_b, mic) = source(500, 480);
        let mut mixer = AudioMixer::with_sources(Some(system), Some(mic), AudioGains::new(100, 100));
        mixer.start(0);
        let emitted = drain(&mut mixer, frames_to_100ns(480));
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].1.len(), 480 * ENCODER_BLOCK_ALIGN / 2);
        assert!(emitted[0].1.iter().all(|&s| s == 1500), "unexpected samples");
    }

    #[test]
    fn a_muted_source_still_mixes_as_silence() {
        // mic_volume 0 normally means the stream is never opened, but a
        // source handed in explicitly must not corrupt the other one.
        let (_a, system) = source(1000, 480);
        let (_b, mic) = source(9000, 480);
        let mut mixer = AudioMixer::with_sources(Some(system), Some(mic), AudioGains::new(100, 0));
        mixer.start(0);
        let emitted = drain(&mut mixer, frames_to_100ns(480));
        assert!(emitted[0].1.iter().all(|&s| s == 1000), "the mic leaked into the mix");
    }

    #[test]
    fn the_shorter_source_bounds_the_emission_and_the_rest_waits() {
        // Both timelines silence-fill to the same target, so this is really a
        // guard: if they ever disagree, the mixer must hold the excess back
        // rather than emit misaligned audio it can never take back.
        let (_a, system) = source(1000, 480);
        let (_b, mic) = source(500, 240);
        let mut mixer = AudioMixer::with_sources(Some(system), Some(mic), AudioGains::new(100, 100));
        mixer.start(0);
        // Pump to exactly the shorter source's extent: no silence fill yet.
        let emitted = drain(&mut mixer, frames_to_100ns(240));
        let frames: usize = emitted.iter().map(|(_, s)| s.len()).sum::<usize>() / (ENCODER_BLOCK_ALIGN / 2);
        assert_eq!(frames, 240, "emitted past the shorter source");
    }

    #[test]
    fn emitted_chunks_are_consecutive() {
        // The sink contract is gapless consecutive chunks: chunk N starts
        // exactly where chunk N-1 ended, or the ring's frame math is wrong.
        let (_a, system) = source(1000, 480);
        let mut mixer = AudioMixer::with_sources(Some(system), None, AudioGains::new(100, 100));
        mixer.start(0);
        let mut emitted = drain(&mut mixer, frames_to_100ns(480));
        emitted.extend(drain(&mut mixer, frames_to_100ns(SAMPLE_RATE as u64)));
        let mut expected_start = 0u64;
        for (start_frame, chunk) in &emitted {
            assert_eq!(*start_frame, expected_start, "a gap opened in the emitted stream");
            expected_start += (chunk.len() / (ENCODER_BLOCK_ALIGN / 2)) as u64;
        }
        assert_eq!(mixer.frames_emitted(), expected_start);
    }

    #[test]
    fn nothing_is_emitted_before_the_timeline_is_anchored() {
        // The anchor is the first video frame's QPC. Emitting before it would
        // put audio at an offset no video frame corresponds to.
        let (_a, system) = source(1000, 480);
        let mut mixer = AudioMixer::with_sources(Some(system), None, AudioGains::new(100, 100));
        assert!(!mixer.started());
        assert!(drain(&mut mixer, frames_to_100ns(480)).is_empty());
    }
}
