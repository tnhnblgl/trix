//! Placing raw capture packets on one continuous timeline.
//!
//! Real packets are placed by their QPC stamps, head/overlap excess is
//! trimmed, idle gaps are filled with synthesized silence, and sub-20 ms
//! timestamp jitter is absorbed without splicing. Sink-agnostic by design:
//! one instance serves system audio, another serves the microphone.

use std::{collections::VecDeque, sync::mpsc::Receiver};

use super::{
    AudioPacket, CONTINUITY_DEAD_BAND_100NS, ENCODER_BLOCK_ALIGN, SAMPLE_RATE, dur_100ns_to_frames,
    frames_to_100ns,
};

/// Turns raw loopback packets into one *continuous* PCM timeline anchored at
/// the first video frame's QPC instant: real packets are placed by their QPC
/// stamps, head/overlap excess is trimmed, idle gaps (loopback goes quiet
/// when nothing renders) are filled with synthesized silence, and sub-20 ms
/// timestamp jitter is absorbed without splicing (the Phase 4 crackle fix).
///
/// The timeline is sink-agnostic: `emit(start_frame, pcm)` receives gapless
/// consecutive chunks — `record` forwards them to the AAC encoder, `replay`
/// appends them to the PCM ring.
pub struct AudioTimeline {
    pending: VecDeque<AudioPacket>,
    t0_qpc: Option<i64>,
    frames_emitted: u64,
    silence_frames_emitted: u64,
    silence: Vec<u8>,

    // splice diagnostics: how often packet placement cut into real audio
    pub micro_gap_events: u64, // silence inserts < 10 ms (should be ~0 during sound)
    pub micro_gap_frames: u64,
    pub large_gap_events: u64, // genuine idle gaps (> 10 ms)
    pub trim_events: u64,      // packets with leading samples dropped
    pub trim_frames: u64,
}

impl AudioTimeline {
    pub fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            t0_qpc: None,
            frames_emitted: 0,
            silence_frames_emitted: 0,
            // 0.5 s of zeroed PCM reused for every silence emission.
            silence: vec![0u8; SAMPLE_RATE / 2 * ENCODER_BLOCK_ALIGN],
            micro_gap_events: 0,
            micro_gap_frames: 0,
            large_gap_events: 0,
            trim_events: 0,
            trim_frames: 0,
        }
    }

    /// Anchors the timeline; the first call wins (t0 = first video frame QPC).
    pub fn start(&mut self, t0_qpc: i64) {
        self.t0_qpc.get_or_insert(t0_qpc);
    }

    pub const fn started(&self) -> bool {
        self.t0_qpc.is_some()
    }

    pub const fn frames_emitted(&self) -> u64 {
        self.frames_emitted
    }

    pub const fn silence_frames_emitted(&self) -> u64 {
        self.silence_frames_emitted
    }

    /// QPC instant (100 ns) up to which audio has been emitted.
    fn emitted_until(&self) -> i64 {
        self.t0_qpc.unwrap_or(0) + frames_to_100ns(self.frames_emitted)
    }

    fn emit(&mut self, bytes: &[u8], sink: &mut impl FnMut(u64, &[u8])) {
        sink(self.frames_emitted, bytes);
        self.frames_emitted += (bytes.len() / ENCODER_BLOCK_ALIGN) as u64;
    }

    fn emit_silence(&mut self, mut frames: u64, sink: &mut impl FnMut(u64, &[u8])) {
        self.silence_frames_emitted += frames;
        while frames > 0 {
            let chunk = frames.min((self.silence.len() / ENCODER_BLOCK_ALIGN) as u64);
            let bytes = chunk as usize * ENCODER_BLOCK_ALIGN;
            let buf = std::mem::take(&mut self.silence);
            self.emit(&buf[..bytes], sink);
            self.silence = buf;
            frames -= chunk;
        }
    }

    /// Places pending loopback packets on the timeline by their QPC stamps
    /// (trimming anything already covered) and fills with silence up to
    /// `target_qpc`.
    pub fn pump(
        &mut self,
        rx: &Receiver<AudioPacket>,
        target_qpc: i64,
        sink: &mut impl FnMut(u64, &[u8]),
    ) {
        if self.t0_qpc.is_none() {
            return;
        }
        while let Ok(packet) = rx.try_recv() {
            self.pending.push_back(packet);
        }

        while let Some(packet) = self.pending.pop_front() {
            let n_frames = (packet.data.len() / ENCODER_BLOCK_ALIGN) as u64;
            let delta = packet.qpc_100ns - self.emitted_until();

            if delta.abs() < CONTINUITY_DEAD_BAND_100NS {
                // Seamless continuation of the flowing stream: append verbatim.
                let data = packet.data;
                self.emit(&data, sink);
            } else if delta > 0 {
                // Real idle gap (loopback went quiet): fill, then append.
                self.large_gap_events += 1;
                self.emit_silence(dur_100ns_to_frames(delta), sink);
                let data = packet.data;
                self.emit(&data, sink);
            } else {
                // Packet substantially overlaps already-covered time (head
                // audio from before recording start, or a device hiccup).
                let skip_frames = dur_100ns_to_frames(-delta).min(n_frames);
                self.trim_events += 1;
                self.trim_frames += skip_frames;
                let bytes = &packet.data[skip_frames as usize * ENCODER_BLOCK_ALIGN..];
                if !bytes.is_empty() {
                    let owned = bytes.to_vec();
                    self.emit(&owned, sink);
                }
            }
        }

        let lag = target_qpc - self.emitted_until();
        if lag > 0 {
            self.emit_silence(dur_100ns_to_frames(lag), sink);
        }
    }

    pub fn log_diagnostics(&self) {
        tracing::info!(
            micro_gap_events = self.micro_gap_events,
            micro_gap_frames = self.micro_gap_frames,
            large_gap_events = self.large_gap_events,
            trim_events = self.trim_events,
            trim_frames = self.trim_frames,
            "audio splice diagnostics"
        );
    }
}
