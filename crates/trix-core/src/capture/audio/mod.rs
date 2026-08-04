//! System-audio and microphone capture.

mod mixer;
mod source;
mod timeline;

pub use mixer::{AudioGains, AudioMixer, apply_gain, mix_into, percent_to_gain};
pub use source::{AudioCapture, AudioSourceKind, record_wav};
pub use timeline::AudioTimeline;

pub const SAMPLE_RATE: usize = 48_000;
pub const CHANNELS: usize = 2;
/// Bytes per interleaved i16 stereo frame (the format fed to the encoder).
pub const ENCODER_BLOCK_ALIGN: usize = CHANNELS * 2;

/// How far audio emission trails the newest video frame. Real packets always
/// emit immediately; only the silence filler holds back this much so a
/// late-arriving real packet is never pre-empted by synthesized silence.
pub const SILENCE_GRACE_100NS: i64 = 1_000_000; // 100 ms

/// Packet QPC stamps jitter by a few samples against the ideal sample-count
/// clock. Within this band a packet is treated as the seamless continuation
/// of the stream (append verbatim — no splice); only discrepancies beyond it
/// are real gaps/overlaps worth re-anchoring for. Splicing on sub-band jitter
/// is audible as constant crackle (518 splices in 5.5 s of continuous tone).
pub const CONTINUITY_DEAD_BAND_100NS: i64 = 200_000; // 20 ms

/// One capture packet: interleaved i16 stereo 48 kHz bytes plus the QPC
/// timestamp (100 ns units) of its first sample.
pub struct AudioPacket {
    pub qpc_100ns: i64,
    pub data: Vec<u8>,
}

pub fn frames_to_100ns(frames: u64) -> i64 {
    (frames as i128 * 10_000_000 / SAMPLE_RATE as i128) as i64
}

pub fn dur_100ns_to_frames(dur: i64) -> u64 {
    (dur.max(0) as i128 * SAMPLE_RATE as i128 / 10_000_000) as u64
}
