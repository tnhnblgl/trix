//! Placing raw capture packets on one continuous timeline.
//!
//! Real packets are placed by their QPC stamps, head/overlap excess is
//! trimmed, idle gaps are filled with synthesized silence, and sub-20 ms
//! timestamp jitter is absorbed without splicing. Sink-agnostic by design:
//! one instance serves system audio, another serves the microphone.

use std::{collections::VecDeque, sync::mpsc::Receiver};

use super::{
    AudioPacket, CONTINUITY_DEAD_BAND_100NS, ENCODER_BLOCK_ALIGN, MAX_SANE_SILENCE_GAP_100NS,
    SAMPLE_RATE, dur_100ns_to_frames, frames_to_100ns,
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
    /// Latches the first time a gap exceeds [`MAX_SANE_SILENCE_GAP_100NS`], so
    /// a device whose clock is durably wrong (every packet, not just one,
    /// would trip the bound) warns once per session rather than once per
    /// packet — a log line per packet inside a capture callback is its own
    /// denial of service.
    discontinuity_warned: bool,

    // splice diagnostics: how often packet placement cut into real audio
    pub micro_gap_events: u64, // silence inserts < 10 ms (should be ~0 during sound)
    pub micro_gap_frames: u64,
    pub large_gap_events: u64, // genuine idle gaps (> 10 ms)
    pub trim_events: u64,      // packets with leading samples dropped
    pub trim_frames: u64,
    pub discontinuity_events: u64, // gaps beyond the sane bound, re-anchored rather than filled
}

impl Default for AudioTimeline {
    fn default() -> Self {
        Self::new()
    }
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
            discontinuity_warned: false,
            micro_gap_events: 0,
            micro_gap_frames: 0,
            large_gap_events: 0,
            trim_events: 0,
            trim_frames: 0,
            discontinuity_events: 0,
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
    /// `target_qpc`. A gap beyond [`MAX_SANE_SILENCE_GAP_100NS`] is treated as
    /// a bad device timestamp rather than a real idle period and is
    /// re-anchored instead of filled — see the comment where that branch is
    /// handled.
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
            } else if delta > 0 && delta <= MAX_SANE_SILENCE_GAP_100NS {
                // Real idle gap (loopback went quiet): fill, then append.
                self.large_gap_events += 1;
                self.emit_silence(dur_100ns_to_frames(delta), sink);
                let data = packet.data;
                self.emit(&data, sink);
            } else if delta > MAX_SANE_SILENCE_GAP_100NS {
                // The packet's timestamp implies a gap no real idle period
                // produces (see MAX_SANE_SILENCE_GAP_100NS) -- almost always a
                // device clock reporting the wrong epoch. Filling it would
                // pin an unbounded amount of synthesized silence in `staged`
                // and, since this runs inside the capture callback, block it
                // for as long as that takes: an effective hang. Re-anchor
                // instead -- resume from here with no fill, exactly like a
                // dead-band continuation, rather than trusting the packet's
                // offset. This cannot desync the two sources or drift them
                // from picture: `t0_qpc` itself (the shared anchor -- the
                // first video frame's QPC) is untouched, and the trailing
                // fill below pulls `emitted_until` back toward real time
                // using `target_qpc`, which comes from the video pacer and
                // which this audio device's clock cannot corrupt.
                self.discontinuity_events += 1;
                if !self.discontinuity_warned {
                    tracing::warn!(
                        delta_100ns = delta,
                        "audio packet timestamp gap exceeds the sane bound, \
                         re-anchoring instead of synthesizing silence for it"
                    );
                    self.discontinuity_warned = true;
                }
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

    pub fn log_diagnostics(&self, source: &str) {
        tracing::info!(
            source,
            micro_gap_events = self.micro_gap_events,
            micro_gap_frames = self.micro_gap_frames,
            large_gap_events = self.large_gap_events,
            trim_events = self.trim_events,
            trim_frames = self.trim_frames,
            discontinuity_events = self.discontinuity_events,
            "audio splice diagnostics"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    /// A packet of `frames` zeroed frames stamped at `qpc_100ns`.
    fn packet(qpc_100ns: i64, frames: u64) -> AudioPacket {
        AudioPacket { qpc_100ns, data: vec![0u8; frames as usize * ENCODER_BLOCK_ALIGN] }
    }

    /// Total frames a `pump` call hands to the sink.
    fn pumped_frames(
        timeline: &mut AudioTimeline,
        rx: &Receiver<AudioPacket>,
        target_qpc: i64,
    ) -> u64 {
        let mut frames = 0u64;
        timeline.pump(rx, target_qpc, &mut |_start, pcm| {
            frames += (pcm.len() / ENCODER_BLOCK_ALIGN) as u64;
        });
        frames
    }

    #[test]
    fn a_gap_within_the_sane_bound_is_still_filled_with_silence() {
        // Regression guard for the existing behaviour: a real, moderate idle
        // gap must still be synthesized, not swept into the discontinuity path.
        let mut timeline = AudioTimeline::new();
        timeline.start(0);
        let (tx, rx) = channel();
        let gap_frames = 48_000; // 1 s -- comfortably real, comfortably under the bound
        tx.send(packet(frames_to_100ns(gap_frames), 480)).unwrap();

        let frames = pumped_frames(&mut timeline, &rx, frames_to_100ns(gap_frames + 480));

        assert_eq!(frames, gap_frames + 480, "the gap must still be silence-filled");
        assert_eq!(timeline.large_gap_events, 1);
        assert_eq!(timeline.discontinuity_events, 0);
    }

    #[test]
    fn a_gap_beyond_the_sane_bound_is_not_synthesized_as_silence() {
        // The failure this finding closes: a device reporting a FILETIME-scale
        // timestamp (the classic QPC/FILETIME mismatch) must not turn into an
        // attempt to synthesize hours of silence. `target_qpc` stays a normal,
        // small value here deliberately -- it comes from the video pacer in
        // production, which this audio packet's bad clock cannot corrupt.
        let mut timeline = AudioTimeline::new();
        timeline.start(0);
        let (tx, rx) = channel();
        let bogus_qpc = MAX_SANE_SILENCE_GAP_100NS.saturating_mul(1000);
        tx.send(packet(bogus_qpc, 480)).unwrap();

        let frames = pumped_frames(&mut timeline, &rx, frames_to_100ns(480));

        assert_eq!(
            frames, 480,
            "only the packet's own frames may be emitted -- the bogus gap must not be synthesized"
        );
        assert_eq!(timeline.large_gap_events, 0, "this is a discontinuity, not a real gap");
        assert_eq!(timeline.discontinuity_events, 1);
    }

    #[test]
    fn a_durably_wrong_clock_warns_once_not_per_packet() {
        // A driver reporting the wrong epoch is wrong on every packet, not
        // just the first -- the warning must not repeat per packet (a log
        // line per packet inside a capture callback is its own denial of
        // service), but the diagnostic counter should still track every
        // occurrence.
        let mut timeline = AudioTimeline::new();
        timeline.start(0);
        let (tx, rx) = channel();
        let bogus_qpc = MAX_SANE_SILENCE_GAP_100NS.saturating_mul(1000);
        tx.send(packet(bogus_qpc, 480)).unwrap();
        tx.send(packet(bogus_qpc + frames_to_100ns(480), 480)).unwrap();

        let frames = pumped_frames(&mut timeline, &rx, frames_to_100ns(960));

        assert_eq!(frames, 960, "both packets' real audio must still reach the sink");
        assert_eq!(timeline.discontinuity_events, 2, "each occurrence is still counted");
        assert!(timeline.discontinuity_warned, "the latch is set after the first occurrence");
    }
}
