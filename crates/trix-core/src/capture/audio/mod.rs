//! System-audio and microphone capture.

mod mixer;
mod source;
mod timeline;

pub use mixer::{AudioGains, AudioMixer};
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

/// A gap between two packets larger than this is a bad timestamp, not a real
/// idle period — the timeline should log it once and re-anchor rather than
/// synthesize silence for it.
///
/// A genuinely idle source (loopback delivers nothing while a game is muted,
/// a microphone nobody is talking into) is self-limiting here: `pump`'s own
/// trailing fill reconciles against `target_qpc` — driven by the *video*
/// pacer, a clock this audio device cannot corrupt — on every call, so a real
/// gap is always caught up to within one pump interval; it can never show up,
/// all at once, as a single packet claiming a huge `delta`. A bad device
/// clock is the opposite shape: the classic case is a driver reporting
/// FILETIME (100 ns ticks since 1601) where QPC (100 ns ticks since boot) was
/// expected, which is off by decades, not seconds. The two failure modes are
/// separated by orders of magnitude, so this bound does not need to be tuned
/// finely — five minutes is comfortably above any pacing jitter or scheduler
/// stall and comfortably below "the clock is simply wrong."
pub const MAX_SANE_SILENCE_GAP_100NS: i64 = 5 * 60 * 10_000_000; // 5 minutes

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
