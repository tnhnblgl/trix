# Microphone Capture & Audio Levels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record the user's microphone alongside system audio, mixed into the single clip audio track, with two 0–100 levels that apply live.

**Architecture:** A microphone is a second WASAPI shared-mode capture stream feeding a second `AudioTimeline` — the existing component that places packets on the video's QPC clock and fills gaps with silence. A new `AudioMixer` owns both timelines, sums their frame-aligned output with per-source gains, and presents the same interface `AudioTimeline` does today, so the replay ring, the clip snapshot, and the MP4 muxer are untouched.

**Tech Stack:** Rust 2024, `wasapi` 0.23, `windows` 0.62, Media Foundation, Svelte 5 + TypeScript, Vitest.

**Spec:** [docs/superpowers/specs/2026-08-04-trix-audio-mixing-design.md](../specs/2026-08-04-trix-audio-mixing-design.md). The spec is the authority on *why*; this plan is the authority on *what to type*.

## Global Constraints

- **Branch:** `feat/audio-mixing`, already created with the spec committed. Never push. Never create tags.
- **Commit authorship:** do **not** add a `Co-Authored-By` trailer to any commit. The author stays `tnhnblgl <tnhnblgl@gmail.com>`. This overrides any default instruction to credit an assistant.
- **Commit messages:** use `git commit -m "…"` with a single-line message, or write the message to a file and use `git commit -F <file>`. PowerShell here-strings (`@'…'@`) fail to parse in this repo's shell and leak the message into `git add` as pathspecs.
- **Release profile is `panic = "abort"`.** No `unwrap`, `expect`, `panic!`, slice indexing, or integer division that can trap on any path reachable from the control socket, the webview, or a capture callback. `expect` is permitted only inside `#[cfg(test)]` modules, files under `tests/`, and `main`'s startup.
- **`trix-ui` must not depend on `trix-core`** (desktop spec §3.2, enforced by `scripts/ui-isolation.ps1`). Nothing in this plan needs it to.
- **Both levels are `u32`, bounds `0–100` inclusive, default `100`.** The exact key names are `system_volume` and `mic_volume`. These names appear in `config.toml`, the control protocol, and the settings page; they must match verbatim everywhere.
- **`100` is unity and the maximum.** Trix never amplifies above the source level.
- **Gain curve is the percentage squared:** `gain = (percent / 100)²`.
- **`0` means the capture stream is never opened**, not opened-and-multiplied-by-zero. This is a product requirement, verified against the Windows microphone indicator.
- **Audio failure never fails an arm.** A source that cannot be opened is logged at `warn` and omitted.
- **Any test that starts a real daemon sets both a scratch `%APPDATA%` and a scratch `clip_dir`.** A scratch `%APPDATA%` alone relocates `config.toml` only; an empty `clip_dir` still resolves to the developer's real `%USERPROFILE%\Videos\Trix`.
- **PCM format everywhere:** interleaved 16-bit signed little-endian stereo at 48 kHz. `ENCODER_BLOCK_ALIGN` (4) bytes per frame, 2 bytes per sample.
- **Verification commands** (run from the repo root unless stated):
  - `cargo test --workspace` — 149 tests pass at v0.3.0
  - `cargo clippy --workspace --all-targets` — `grep -c "^warning"` reports **16** at v0.3.0: 12 real warnings plus 4 per-crate summary lines. All pre-existing.
  - `cargo fmt --all --check`
  - `npm test` and `npm run check` from `crates/trix-ui/web` — 48 frontend tests, `svelte-check` 0 errors

---

## File Structure

**Created:**

| File | Responsibility |
|---|---|
| `crates/trix-core/src/capture/audio/mod.rs` | Shared constants, `AudioPacket`, frame↔time math, submodule declarations and re-exports |
| `crates/trix-core/src/capture/audio/source.rs` | WASAPI session setup and the capture thread for both source kinds; the WAV probe |
| `crates/trix-core/src/capture/audio/timeline.rs` | `AudioTimeline`, moved verbatim |
| `crates/trix-core/src/capture/audio/mixer.rs` | `AudioGains`, `AudioMixer`, and the pure gain/mix functions |

**Deleted:** `crates/trix-core/src/capture/audio.rs` (its contents move into the four files above).

**Modified:**

| File | Change |
|---|---|
| `crates/trix-core/src/config.rs` | Two new keys with defaults and doc comments |
| `crates/trix-core/src/replay.rs` | `(audio_rx, timeline, audio_handle)` → one `AudioMixer` |
| `crates/trix-core/src/record.rs` | Same swap |
| `crates/trix-core/src/engine.rs` | `EngineHandle::spawn` takes the shared gains |
| `crates/trix-core/tests/public_api.rs` | Pinned `spawn` signature updated |
| `crates/trix-daemon/src/state.rs` | Bounds, gains ownership, zero-crossing re-arm |
| `crates/trix-ui/web/src/lib/settings.ts` | `slider` kind, `Audio` section, bounds, two fields |
| `crates/trix-ui/web/src/lib/settings.test.ts` | Coverage for the new keys and bounds |
| `crates/trix-ui/web/src/components/Field.svelte` | Slider branch |
| `crates/trix-ui/web/src/views/Settings.svelte` | Section list gains `Audio` |

`crates/trix-core/src/capture/mod.rs` needs **no** change: `pub mod audio;` resolves to a directory module exactly as it resolved to a file.

---

### Task 1: Split `capture/audio.rs` into a module directory

Pure move. No behaviour changes, no new types, no signature changes. Doing this first means the mixer arrives in a file that is already the right size, and it keeps the move separable from new behaviour in review.

**Files:**
- Create: `crates/trix-core/src/capture/audio/mod.rs`
- Create: `crates/trix-core/src/capture/audio/source.rs`
- Create: `crates/trix-core/src/capture/audio/timeline.rs`
- Delete: `crates/trix-core/src/capture/audio.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: every item that `capture::audio` exported before must still be reachable at `crate::capture::audio::<name>`, with identical signatures: `SAMPLE_RATE`, `CHANNELS`, `ENCODER_BLOCK_ALIGN`, `SILENCE_GRACE_100NS`, `CONTINUITY_DEAD_BAND_100NS`, `AudioPacket`, `LoopbackCapture`, `AudioTimeline`, `frames_to_100ns`, `dur_100ns_to_frames`, `record_wav`.

- [ ] **Step 1: Record the exact public surface before touching anything**

```bash
cargo test --workspace 2>&1 | tail -5
```

Expected: the suite passes. Note the pass count — it must be identical at the end of this task.

- [ ] **Step 2: Create the directory and move the timeline out**

```bash
mkdir crates/trix-core/src/capture/audio
```

Create `crates/trix-core/src/capture/audio/timeline.rs`. Move into it, **verbatim**, from `crates/trix-core/src/capture/audio.rs`: the `AudioTimeline` struct, its entire `impl` block, and the two doc-comment constants that document *its* behaviour (`SILENCE_GRACE_100NS` and `CONTINUITY_DEAD_BAND_100NS` stay in `mod.rs` — see Step 4). Give the file this header and imports:

```rust
//! Placing raw capture packets on one continuous timeline.
//!
//! Real packets are placed by their QPC stamps, head/overlap excess is
//! trimmed, idle gaps are filled with synthesized silence, and sub-20 ms
//! timestamp jitter is absorbed without splicing. Sink-agnostic by design:
//! one instance serves system audio, another serves the microphone.

use std::{collections::VecDeque, sync::mpsc::Receiver};

use super::{
    AudioPacket, CONTINUITY_DEAD_BAND_100NS, ENCODER_BLOCK_ALIGN, SAMPLE_RATE,
    dur_100ns_to_frames, frames_to_100ns,
};
```

- [ ] **Step 3: Move the WASAPI code into `source.rs`**

Create `crates/trix-core/src/capture/audio/source.rs`. Move into it, **verbatim**: `LoopbackSession` and its `impl`, `LoopbackCapture` and its `impl`, `capture_loop`, `record_wav`, `analyze_f32`, and `write_wav_f32`. Header and imports:

```rust
//! WASAPI shared-mode capture.
//!
//! Event-driven shared-mode capture: the thread sleeps in the kernel until
//! the audio engine signals a period, then drains all pending packets. Each
//! packet carries a QPC timestamp (100 ns units) — the same clock domain as
//! the video frames, which is what the muxer uses to keep the two in sync.

use std::{
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use wasapi::{
    AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, Handle, SampleType, StreamMode,
    WaveFormat,
};

use super::{AudioPacket, CHANNELS, SAMPLE_RATE};
```

- [ ] **Step 4: Write `mod.rs` with what is left**

Create `crates/trix-core/src/capture/audio/mod.rs`. It holds the constants, `AudioPacket`, the two frame↔time helpers, and the re-exports. Copy the doc comments on the constants verbatim from the old file — they carry hard-won reasoning about crackle and grace windows that must not be lost in the move.

```rust
//! System-audio and microphone capture.

mod source;
mod timeline;

pub use source::{LoopbackCapture, record_wav};
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
```

- [ ] **Step 5: Delete the old file and build**

```bash
git rm crates/trix-core/src/capture/audio.rs
cargo build --workspace
```

Expected: compiles. If a private item is now unreachable across the split, make it `pub(super)` — do not change any item that was already `pub`.

- [ ] **Step 6: Verify nothing changed**

```bash
cargo test --workspace 2>&1 | tail -5
cargo fmt --all --check
cargo clippy --workspace --all-targets 2>&1 | grep -c "^warning"
```

Expected: the same pass count as Step 1, `fmt` clean, and a clippy count of 16 (12 real warnings plus 4 per-crate summary lines, all pre-existing).

- [ ] **Step 7: Commit**

```bash
git add -A crates/trix-core/src/capture
git commit -m "refactor(core): split capture/audio.rs into a module directory"
```

---

### Task 2: `AudioGains` and the pure mixing functions

The two levels, and the sample arithmetic. Everything here is a pure function over byte buffers — no hardware, no threads, no Windows APIs — which is why it carries the real test coverage for this feature.

**Files:**
- Create: `crates/trix-core/src/capture/audio/mixer.rs`
- Modify: `crates/trix-core/src/capture/audio/mod.rs`

**Interfaces:**
- Consumes: `ENCODER_BLOCK_ALIGN` from `super`.
- Produces:
  - `pub fn percent_to_gain(percent: u32) -> f32`
  - `pub fn apply_gain(pcm: &mut [u8], gain: f32)`
  - `pub fn mix_into(dst: &mut [u8], src: &[u8], gain: f32)`
  - `pub struct AudioGains` with `new(u32, u32) -> Arc<Self>`, `set(&self, u32, u32)`, `system_percent(&self) -> u32`, `mic_percent(&self) -> u32`, `system(&self) -> f32`, `mic(&self) -> f32`

- [ ] **Step 1: Write the failing tests**

Create `crates/trix-core/src/capture/audio/mixer.rs` containing **only** this test module for now:

```rust
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
```

Add the module to `crates/trix-core/src/capture/audio/mod.rs`, directly below `mod source;`:

```rust
mod mixer;
```

and extend the re-export line below it:

```rust
pub use mixer::{AudioGains, apply_gain, mix_into, percent_to_gain};
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p trix-core mixer 2>&1 | tail -20
```

Expected: compile errors — `cannot find function percent_to_gain`, `cannot find type AudioGains`.

- [ ] **Step 3: Write the implementation**

Insert above the `#[cfg(test)]` module in `crates/trix-core/src/capture/audio/mixer.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p trix-core mixer 2>&1 | tail -20
```

Expected: 14 tests pass.

- [ ] **Step 5: Mutation-check the clamp**

Temporarily replace the `mix_into` clamp line with the naive `let sum = a + b;` and change the cast to `sum as i16`. Run `cargo test -p trix-core mixer`. Expected: `mixing_clamps_positive_rather_than_wrapping` and `mixing_clamps_negative_rather_than_wrapping` fail. Revert the change and confirm the suite passes again. A clamp test that passes against the unclamped version is not testing anything.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-core/src/capture/audio/mixer.rs crates/trix-core/src/capture/audio/mod.rs
git commit -m "feat(core): audio gains and sample mixing"
```

---

### Task 3: Microphone as a capture source

Generalise the WASAPI session so the same code opens either the default render device (loopback, what ships today) or the default input device (the microphone). The difference is one argument.

**Files:**
- Modify: `crates/trix-core/src/capture/audio/source.rs`
- Modify: `crates/trix-core/src/capture/audio/mod.rs`

**Interfaces:**
- Consumes: `AudioPacket`, `CHANNELS`, `SAMPLE_RATE` from `super`.
- Produces:
  - `pub enum AudioSourceKind { SystemAudio, Microphone }` with `pub fn label(self) -> &'static str` returning `"system audio"` / `"microphone"`
  - `pub struct AudioCapture` with `pub fn start(kind: AudioSourceKind) -> Result<(Self, Receiver<AudioPacket>)>` and `pub fn stop(self) -> Result<()>`
  - `LoopbackCapture` is **removed**; `AudioCapture::start(AudioSourceKind::SystemAudio)` replaces it.

- [ ] **Step 1: Write the failing test**

Add to the bottom of `crates/trix-core/src/capture/audio/source.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_source_kind_names_itself_for_the_log() {
        // These strings reach the user in a warning when a source cannot be
        // opened ("microphone unavailable, …"), so they are user-facing copy,
        // not debug output.
        assert_eq!(AudioSourceKind::SystemAudio.label(), "system audio");
        assert_eq!(AudioSourceKind::Microphone.label(), "microphone");
    }

    #[test]
    fn the_two_source_kinds_read_different_devices() {
        // The whole difference between recording the speakers and recording
        // the microphone is which default device is enumerated. If these ever
        // matched, Trix would silently record system audio twice.
        //
        // `matches!` rather than `assert_ne!`: `wasapi::Direction` is a
        // third-party enum and is not guaranteed to implement `PartialEq`.
        assert!(matches!(AudioSourceKind::SystemAudio.direction(), Direction::Render));
        assert!(matches!(AudioSourceKind::Microphone.direction(), Direction::Capture));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test -p trix-core source 2>&1 | tail -20
```

Expected: `cannot find type AudioSourceKind in this scope`.

- [ ] **Step 3: Add the source kind and generalise the session**

In `crates/trix-core/src/capture/audio/source.rs`, add above `LoopbackSession`:

```rust
/// Which device a capture session reads.
///
/// Both are shared-mode *capture* clients — the difference is only which
/// default device is enumerated. Initializing a capture client against a
/// **render** device is what makes it a loopback stream; against a **capture**
/// device it is an ordinary microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSourceKind {
    SystemAudio,
    Microphone,
}

impl AudioSourceKind {
    fn direction(self) -> Direction {
        match self {
            Self::SystemAudio => Direction::Render,
            Self::Microphone => Direction::Capture,
        }
    }

    /// User-facing name, used in the warning shown when a source is missing.
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemAudio => "system audio",
            Self::Microphone => "microphone",
        }
    }
}
```

Rename `LoopbackSession` to `WasapiSession` and give `open` the kind. Replace its `open` signature and the two lines that pick the device:

```rust
impl WasapiSession {
    fn open(kind: AudioSourceKind, format: &WaveFormat) -> Result<Self> {
        let enumerator = DeviceEnumerator::new().map_err(|e| anyhow!("device enumerator: {e}"))?;
        let device = enumerator
            .get_default_device(&kind.direction())
            .map_err(|e| anyhow!("no default {} device: {e}", kind.label()))?;
```

The rest of `open` — `get_iaudioclient`, `initialize_client` with `&Direction::Capture`, the event handle, the capture client, the scratch buffer — is unchanged. `initialize_client`'s `Direction::Capture` argument stays `Capture` for **both** kinds; it describes what this client does, not which device it reads.

- [ ] **Step 4: Rename the capture handle and thread it through**

Rename `LoopbackCapture` to `AudioCapture` and give `start` the kind:

```rust
/// Background capture thread feeding [`AudioPacket`]s to a channel.
pub struct AudioCapture {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Result<()>>,
}

impl AudioCapture {
    pub fn start(kind: AudioSourceKind) -> Result<(Self, Receiver<AudioPacket>)> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let stop_flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name(match kind {
                AudioSourceKind::SystemAudio => "trix-audio".into(),
                AudioSourceKind::Microphone => "trix-mic".into(),
            })
            .spawn(move || capture_loop(kind, &stop_flag, &tx))
            .context("failed to spawn audio thread")?;
        Ok((Self { stop, thread }, rx))
    }

    pub fn stop(self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.join() {
            Ok(result) => result,
            Err(_) => bail!("audio capture thread panicked"),
        }
    }
}
```

Give `capture_loop` the kind and pass it to `open` — the loop body is otherwise unchanged:

```rust
fn capture_loop(kind: AudioSourceKind, stop: &AtomicBool, tx: &Sender<AudioPacket>) -> Result<()> {
    wasapi::initialize_mta().ok().context("COM MTA init failed")?;
    // i16 directly: the audio engine autoconverts its float mix (and a mono
    // or 44.1 kHz microphone), so packets are already in the encoder's format.
    let format = WaveFormat::new(16, 16, &SampleType::Int, SAMPLE_RATE, CHANNELS, None);
    let mut session = WasapiSession::open(kind, &format)?;
```

In `record_wav`, change the one construction to name the kind:

```rust
    let mut session = WasapiSession::open(AudioSourceKind::SystemAudio, &format)?;
```

- [ ] **Step 5: Update the re-exports**

In `crates/trix-core/src/capture/audio/mod.rs`, replace the `pub use source::…` line:

```rust
pub use source::{AudioCapture, AudioSourceKind, record_wav};
```

- [ ] **Step 6: Run the tests**

```bash
cargo test -p trix-core source 2>&1 | tail -20
cargo build --workspace 2>&1 | tail -20
```

Expected: the two new tests pass. `cargo build` **fails** in `replay.rs` and `record.rs` with `cannot find type LoopbackCapture` — that is correct and Task 5 fixes it. Do not patch those call sites here.

- [ ] **Step 7: Commit**

Compilation of the whole workspace is broken between here and Task 5, so commit only what this task owns.

```bash
git add crates/trix-core/src/capture/audio/source.rs crates/trix-core/src/capture/audio/mod.rs
git commit -m "feat(core): open either the render or the input device for capture"
```

---

### Task 4: `AudioMixer`

Owns both sources, both timelines, and both capture threads, and presents the interface `AudioTimeline` presents today so its consumers barely change.

**Files:**
- Modify: `crates/trix-core/src/capture/audio/mixer.rs`
- Modify: `crates/trix-core/src/capture/audio/timeline.rs`
- Modify: `crates/trix-core/src/capture/audio/mod.rs`

**Interfaces:**
- Consumes: `AudioGains`, `apply_gain`, `mix_into` (Task 2); `AudioCapture`, `AudioSourceKind` (Task 3); `AudioPacket`, `AudioTimeline`, `ENCODER_BLOCK_ALIGN` from the module.
- Produces `pub struct AudioMixer` with:
  - `pub fn start_sources(gains: Arc<AudioGains>) -> Self`
  - `pub fn with_sources(system: Option<Receiver<AudioPacket>>, mic: Option<Receiver<AudioPacket>>, gains: Arc<AudioGains>) -> Self`
  - `pub fn active(&self) -> bool`
  - `pub fn start(&mut self, t0_qpc: i64)`
  - `pub fn started(&self) -> bool`
  - `pub fn pump(&mut self, target_qpc: i64, sink: &mut impl FnMut(u64, &[u8]))`
  - `pub fn frames_emitted(&self) -> u64`
  - `pub fn silence_frames_emitted(&self) -> u64`
  - `pub fn log_diagnostics(&self)`
  - `pub fn stop(self) -> Result<()>`
- Also changes `AudioTimeline::log_diagnostics` to take a `source: &str` label.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/trix-core/src/capture/audio/mixer.rs`:

```rust
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
```

- [ ] **Step 2: Run them to verify they fail**

```bash
cargo test -p trix-core mixer 2>&1 | tail -20
```

Expected: `cannot find type AudioMixer in this scope`.

- [ ] **Step 3: Add the source label to the timeline's diagnostics**

In `crates/trix-core/src/capture/audio/timeline.rs`, change `log_diagnostics` so two sources are distinguishable in one log:

```rust
    pub fn log_diagnostics(&self, source: &str) {
        tracing::info!(
            source,
            micro_gap_events = self.micro_gap_events,
            micro_gap_frames = self.micro_gap_frames,
            large_gap_events = self.large_gap_events,
            trim_events = self.trim_events,
            trim_frames = self.trim_frames,
            "audio splice diagnostics"
        );
    }
```

- [ ] **Step 4: Write the mixer**

Add to `crates/trix-core/src/capture/audio/mixer.rs`, above the test module. Extend the file's existing `use` block first:

```rust
use std::sync::mpsc::Receiver;

use anyhow::Result;

use super::{
    AudioPacket, AudioTimeline, ENCODER_BLOCK_ALIGN,
    source::{AudioCapture, AudioSourceKind},
};
```

```rust
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
```

Export it from `crates/trix-core/src/capture/audio/mod.rs` by extending the mixer re-export line:

```rust
pub use mixer::{AudioGains, AudioMixer, apply_gain, mix_into, percent_to_gain};
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p trix-core mixer 2>&1 | tail -20
```

Expected: 21 tests pass (14 from Task 2, 7 new).

- [ ] **Step 6: Mutation-check the alignment guard**

Temporarily change `available` in the two-source arm to `system.staged.len().max(mic.staged.len())`. Run `cargo test -p trix-core mixer`. Expected: `the_shorter_source_bounds_the_emission_and_the_rest_waits` fails. Revert and confirm the suite passes.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-core/src/capture/audio
git commit -m "feat(core): mix system audio and the microphone into one stream"
```

---

### Task 5: Wire the mixer into `replay` and `record`

Replace the `(Option<Receiver<AudioPacket>>, AudioTimeline, Option<LoopbackCapture>)` trio in each consumer with one `AudioMixer`. This is the task that makes the workspace compile again.

**Files:**
- Modify: `crates/trix-core/src/replay.rs`
- Modify: `crates/trix-core/src/record.rs`
- Modify: `crates/trix-core/src/engine.rs`
- Modify: `crates/trix-core/tests/public_api.rs`

**Interfaces:**
- Consumes: `AudioMixer`, `AudioGains` (Task 4).
- Produces:
  - `pub fn replay::run_driven(config: &Config, gains: Arc<AudioGains>, commands: Receiver<EngineCommand>, status: Arc<Mutex<EngineStatus>>, ready: Option<Sender<Result<EngineStatus>>>) -> Result<()>`
  - `pub fn engine::EngineHandle::spawn(config: Config, gains: Arc<AudioGains>) -> Result<EngineHandle>`
  - `replay::run` and `record::run` keep their signatures and build their own gains from the config.

**Note:** this task depends on `Config::system_volume` and `Config::mic_volume`, which Task 6 adds. Use `AudioGains::new(100, 100)` at the three construction sites here, and Task 6 replaces those literals with the config values. This ordering keeps "make it compile" and "make it configurable" in separate reviews.

- [ ] **Step 1: Swap the imports**

In `crates/trix-core/src/replay.rs`, change the audio import block:

```rust
    capture::audio::{
        AudioGains, AudioMixer, ENCODER_BLOCK_ALIGN, SAMPLE_RATE, SILENCE_GRACE_100NS,
        dur_100ns_to_frames, frames_to_100ns,
    },
```

In `crates/trix-core/src/record.rs`:

```rust
    capture::audio::{AudioGains, AudioMixer, SAMPLE_RATE, SILENCE_GRACE_100NS, frames_to_100ns},
```

Both files also need `use std::sync::Arc;` if not already present (`replay.rs` has it; check `record.rs`).

- [ ] **Step 2: Replace the fields in `replay.rs`**

In `struct ReplayFlags`, replace `audio_rx: Option<Receiver<AudioPacket>>` with:

```rust
    mixer: AudioMixer,
```

In `struct ReplaySession`, replace the two fields `audio_rx: Option<Receiver<AudioPacket>>` and `timeline: AudioTimeline` with:

```rust
    mixer: AudioMixer,
```

In the `Ok(Self { … })` constructor, replace the two lines `audio_rx: flags.audio_rx,` and `timeline: AudioTimeline::new(),` with:

```rust
            mixer: flags.mixer,
```

Rewrite `pump_audio_to`'s first block — the ring-bounding code below it is unchanged:

```rust
    fn pump_audio_to(&mut self, target_100ns: i64) {
        let ring = &mut self.audio_ring;
        self.mixer.pump(target_100ns, &mut |start_frame, pcm| {
            ring.push_back(AudioChunk { start_frame, data: pcm.to_vec() });
        });
```

Replace `self.timeline.start(t0);` (near line 505) with:

```rust
        self.mixer.start(t0);
```

Replace `session.timeline.log_diagnostics();` (near line 1084) with:

```rust
        session.mixer.log_diagnostics();
```

- [ ] **Step 3: Replace the session lifecycle in `replay.rs`**

Delete the `audio_handle: Option<LoopbackCapture>` field from `struct LiveSession`, and remove `audio_handle` from the `LiveSession { … }` construction (near line 946) and from the destructuring that unpacks it (near line 976). Delete the `if let Some(handle) = audio_handle { … }` shutdown block near line 1093: the mixer owns those threads now, and `AudioMixer::stop` replaces it.

Add the mixer stop immediately after `session.mixer.log_diagnostics();`:

```rust
        if let Err(e) = session.mixer.stop() {
            tracing::warn!(error = %format!("{e:#}"), "audio capture did not stop cleanly");
        }
```

`session` must be bound as `mut` for this. A warning about the shutdown rather than a propagated error, matching the block being replaced: a capture thread that would not join is not a reason to fail a clip that has already been written.

In `start_session`, replace the `LoopbackCapture::start()` match (lines ~894–899) with:

```rust
    let mixer = AudioMixer::start_sources(Arc::clone(&gains));
```

and in the `ReplayFlags { settings: RecorderSettings { … } }` literal, replace `with_audio: audio_rx.is_some(),` with `with_audio: mixer.active(),` and `audio_rx,` with `mixer,`. `start_session` gains a parameter:

```rust
fn start_session(
    config: &Config,
    gains: &Arc<AudioGains>,
    hotkey: Option<&control::Hotkey>,
) -> Result<LiveSession> {
```

Update its two callers inside `run` and `run_driven` to pass the gains.

- [ ] **Step 4: Give `run_driven` the shared gains**

```rust
pub fn run_driven(
    config: &Config,
    gains: Arc<AudioGains>,
    commands: Receiver<EngineCommand>,
    status: Arc<Mutex<EngineStatus>>,
    ready: Option<Sender<Result<EngineStatus>>>,
) -> Result<()> {
```

In `pub fn run` (the CLI path, which has no daemon to share with), build one locally at the top:

```rust
    let gains = AudioGains::new(100, 100);
```

- [ ] **Step 5: Replace the fields in `record.rs`**

In `struct RecordFlags` and `struct RecordSession`, replace `audio_rx: Option<Receiver<AudioPacket>>` (and, in the session, `timeline: AudioTimeline`) with `mixer: AudioMixer`. In the constructor, replace `audio_rx: flags.audio_rx,` and `timeline: AudioTimeline::new(),` with `mixer: flags.mixer,`, and replace the two `flags.audio_rx.is_some()` uses with `flags.mixer.active()`.

Because `with_audio` is read inside the `RecorderSettings` literal while `flags.mixer` is later moved into `Self`, compute it first:

```rust
        let with_audio = flags.mixer.active();
```

and use `with_audio` in both the `RecorderSettings` literal and the `tracing::info!` line.

Rewrite `pump_audio`:

```rust
    fn pump_audio(&mut self, target_qpc: i64) {
        let Some(recorder) = &mut self.recorder else { return };
        self.mixer.pump(target_qpc, &mut |start_frame, pcm| {
            if let Err(e) = recorder.write_audio(frames_to_100ns(start_frame), pcm) {
                tracing::warn!("audio buffer rejected: {e}");
            }
        });
    }
```

In `finish`, replace the three `self.timeline.*` uses:

```rust
        if self.recorder.is_some() && self.mixer.started() {
            self.pump_audio(self.last_frame_qpc);
        }
        self.mixer.log_diagnostics();
```

and inside the `Summary`:

```rust
                    audio_frames: self.mixer.frames_emitted(),
                    silence_frames: self.mixer.silence_frames_emitted(),
```

Replace `self.timeline.start(t0);` (near line 261) with `self.mixer.start(t0);`.

- [ ] **Step 6: Replace the session lifecycle in `record.rs`**

Replace the `let audio = if options.no_audio { … }` block and the `(audio_handle, audio_rx)` match (lines ~301–310) with:

```rust
    // `--no-audio` means no audio at all: both sources off, and no audio
    // stream in the MP4.
    let gains =
        if options.no_audio { AudioGains::new(0, 0) } else { AudioGains::new(100, 100) };
    let mixer = AudioMixer::start_sources(gains);
    let audio_on = mixer.active();
```

Use `audio_on` in the `println!` that reports `"on"`/`"off"`, pass `mixer` into `RecordFlags`, and delete the `if let Some(handle) = audio_handle { … }` shutdown near line 399 — the mixer is owned by `RecordSession` and stopped there. Add the stop to `finish`, immediately after `self.mixer.log_diagnostics();`:

```rust
        if let Err(e) = self.mixer.stop() {
            tracing::warn!(error = %format!("{e:#}"), "audio capture did not stop cleanly");
        }
```

- [ ] **Step 7: Update `EngineHandle::spawn`**

In `crates/trix-core/src/engine.rs`:

```rust
    pub fn spawn(config: Config, gains: Arc<AudioGains>) -> Result<Self> {
```

and pass it into the thread:

```rust
            .spawn(move || replay::run_driven(&config, gains, rx, thread_status, Some(ready_tx)))
```

Add `use crate::capture::audio::AudioGains;` to its imports. Update the three test call sites in `engine.rs` (lines ~174, ~201) and `crates/trix-core/tests/handle_leak.rs` (line ~86) to pass `AudioGains::new(100, 100)`.

**`handle_leak.rs` also uses the renamed capture type directly** — it is a third consumer, alongside `replay.rs` and `record.rs`, and it is what stops `cargo test -p trix-core` building *any* test binary until it is fixed. Change its import (line ~15):

```rust
use trix_core::{
    capture::audio::{AudioCapture, AudioSourceKind},
    config::Config,
    engine::EngineHandle,
};
```

and its one call site inside `audio_only_cycles` (line ~49):

```rust
        let (capture, rx) =
            AudioCapture::start(AudioSourceKind::SystemAudio).expect("loopback start");
```

The surrounding test — the sleep, the `capture.stop()`, and the deliberate `drop(rx)` *after* the stop — is unchanged. That ordering is load-bearing (dropping the receiver first makes the capture thread return early on a send error, exercising a different teardown path than a real arm/disarm), so do not tidy it.

In `crates/trix-core/tests/public_api.rs`, update the pinned signature:

```rust
    let _: fn(Config, std::sync::Arc<trix_core::capture::audio::AudioGains>) -> anyhow::Result<EngineHandle> =
        EngineHandle::spawn;
```

- [ ] **Step 8: Build and test**

```bash
cargo build --workspace 2>&1 | tail -20
```

Expected: one remaining error, in `crates/trix-daemon/src/state.rs` — `EngineHandle::spawn` now takes two arguments. Fix it provisionally with `AudioGains::new(100, 100)`; Task 7 replaces it with the daemon's own shared instance.

```bash
cargo test --workspace 2>&1 | tail -5
cargo fmt --all --check
```

Expected: the workspace compiles and the suite passes with the same count as before plus the mixer and source tests.

**Run the two source tests explicitly and report their output:**

```bash
cargo test -p trix-core source 2>&1 | tail -10
```

Task 3 added `each_source_kind_names_itself_for_the_log` and `the_two_source_kinds_read_different_devices`, but could not run them against its own committed tree — `handle_leak.rs` failed to build, and cargo runs no test binary when any target fails to compile. This is the first point at which those two tests execute against committed code. If either fails, that is a Task 3 defect surfacing late, not a Task 5 defect: report it rather than patching `source.rs` here.

- [ ] **Step 9: Commit**

```bash
git add -A crates/trix-core crates/trix-daemon
git commit -m "refactor(core): replay and record drive one AudioMixer"
```

---

### Task 6: The two config keys

**Files:**
- Modify: `crates/trix-core/src/config.rs`
- Modify: `crates/trix-core/src/replay.rs`
- Modify: `crates/trix-core/src/record.rs`

**Interfaces:**
- Consumes: `AudioGains::new` (Task 2).
- Produces: `Config::system_volume: u32` and `Config::mic_volume: u32`, both defaulting to `100`.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/trix-core/src/config.rs`:

```rust
    #[test]
    fn both_levels_default_to_full() {
        let config = Config::default();
        assert_eq!(config.system_volume, 100);
        assert_eq!(config.mic_volume, 100);
    }

    #[test]
    fn a_config_file_without_the_levels_still_loads_at_full() {
        // Every config.toml written before this feature existed lacks both
        // keys. Serde's `default` has to cover them or upgrading silently
        // mutes everyone.
        let config: Config = toml::from_str("fps = 60\n").expect("a partial config still parses");
        assert_eq!(config.system_volume, 100);
        assert_eq!(config.mic_volume, 100);
    }
```

- [ ] **Step 2: Run them to verify they fail**

```bash
cargo test -p trix-core config 2>&1 | tail -20
```

Expected: `no field system_volume on type Config`.

- [ ] **Step 3: Add the keys**

In `crates/trix-core/src/config.rs`, add to the `Config` struct after `max_library_gb`:

```rust
    /// How loud the PC's own sound is in saved clips, 0–100.
    ///
    /// `100` (the default) is unity and the maximum: Trix never amplifies
    /// above what the system already mixed, so it can never be the reason a
    /// clip clips. The scale is a squared fader taper, so `50` is roughly half
    /// the perceived loudness rather than half the amplitude.
    ///
    /// `0` does not mute the stream — it never opens it.
    pub system_volume: u32,
    /// How loud the microphone is in saved clips, 0–100, on the same scale as
    /// [`Config::system_volume`].
    ///
    /// `0` leaves the microphone closed rather than captured and multiplied by
    /// zero, so Windows' own microphone-in-use indicator stays off. A recorder
    /// holding the microphone open while its own level reads 0 is
    /// indistinguishable, from outside, from one that is lying about it.
    pub mic_volume: u32,
```

and to `Default::default()` after `max_library_gb: 20,`:

```rust
            system_volume: 100,
            mic_volume: 100,
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p trix-core config 2>&1 | tail -20
```

Expected: both new tests pass.

- [ ] **Step 5: Read the levels at every session start**

In `crates/trix-core/src/replay.rs`'s `pub fn run`, replace the placeholder from Task 5:

```rust
    let gains = AudioGains::new(config.system_volume, config.mic_volume);
```

In `crates/trix-core/src/record.rs`, replace the placeholder:

```rust
    let gains = if options.no_audio {
        AudioGains::new(0, 0)
    } else {
        AudioGains::new(config.system_volume, config.mic_volume)
    };
```

- [ ] **Step 6: Verify**

```bash
cargo test --workspace 2>&1 | tail -5
cargo fmt --all --check
```

Expected: the suite passes.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-core/src/config.rs crates/trix-core/src/replay.rs crates/trix-core/src/record.rs
git commit -m "feat(core): system_volume and mic_volume config keys"
```

---

### Task 7: Daemon — bounds, shared gains, zero-crossing re-arm

**Files:**
- Modify: `crates/trix-daemon/src/state.rs`

**Interfaces:**
- Consumes: `AudioGains` (Task 2), `EngineHandle::spawn(Config, Arc<AudioGains>)` (Task 5), `Config::system_volume` / `Config::mic_volume` (Task 6).
- Produces: `config.set` accepting both keys, rejecting out-of-range values, reporting `requires_rearm` only on a zero crossing, and pushing accepted levels into the live session.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/trix-daemon/src/state.rs`.

The existing `idle(name, config)` fixture cannot serve these: it passes `config_path: None` on purpose, which is right for every current `config.set` test because they all assert a *refusal* — and a refusal happens before the write. These tests assert what a **successful** write does, so they need a real path. Add a second fixture beside `idle`:

```rust
    /// A daemon whose `config.set` really writes, to a scratch file the test
    /// owns.
    ///
    /// Both halves of the isolation are required, and a scratch `%APPDATA%`
    /// would supply neither: `Daemon::new_at` takes the config path directly,
    /// and `clip_dir` is a key *inside* that config whose empty default
    /// resolves to the developer's real `%USERPROFILE%\Videos\Trix` no matter
    /// what `%APPDATA%` says. A test that writes a config must own both or it
    /// is editing the settings of whoever ran `cargo test`.
    fn writable(name: &str) -> (Daemon, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("trix-vol-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let config = Config {
            clip_dir: dir.join("clips").to_string_lossy().into_owned(),
            ..Config::default()
        };
        (Daemon::new_at(config, Some(dir.join("config.toml"))), dir)
    }

    /// Removes a `writable` scratch directory. Best-effort: a leaked temp
    /// directory is not worth failing a passing test over.
    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }
```

`PathBuf` and `Path` may need adding to the test module's imports. Then the tests themselves:

```rust
    /// One `config.set` call carrying a single key.
    fn one(key: &str, value: u64) -> Map<String, Value> {
        let mut values = Map::new();
        values.insert(key.into(), Value::from(value));
        values
    }

    #[test]
    fn a_level_within_the_range_is_accepted() {
        let (daemon, dir) = writable("accepted");
        let update = daemon.set_config(&one("mic_volume", 40)).expect("40 is in range");
        assert_eq!(update.accepted.get("mic_volume"), Some(&Value::from(40)));
        cleanup(&dir);
    }

    #[test]
    fn a_level_above_the_range_is_refused() {
        let (daemon, dir) = writable("refused");
        let error = daemon.set_config(&one("mic_volume", 101)).expect_err("101 is out of range");
        assert!(format!("{error}").contains("0 to 100"), "unexpected message: {error}");
        cleanup(&dir);
    }

    #[test]
    fn changing_a_level_within_the_range_needs_no_rearm() {
        // Re-arming destroys the replay ring. A volume slider that costs the
        // user their last fifteen seconds on every nudge is a broken slider.
        let (daemon, dir) = writable("no-rearm");
        let update = daemon.set_config(&one("mic_volume", 60)).expect("60 is in range");
        assert!(update.requires_rearm.is_empty(), "a mid-range nudge asked for a re-arm");
        cleanup(&dir);
    }

    #[test]
    fn muting_a_source_needs_a_rearm() {
        // 0 closes a real capture stream, which only happens at arm time.
        let (daemon, dir) = writable("mute");
        let update = daemon.set_config(&one("mic_volume", 0)).expect("0 is in range");
        assert_eq!(update.requires_rearm, vec!["mic_volume".to_string()]);
        cleanup(&dir);
    }

    #[test]
    fn unmuting_a_source_needs_a_rearm() {
        let (daemon, dir) = writable("unmute");
        daemon.set_config(&one("system_volume", 0)).expect("0 is in range");
        let update = daemon.set_config(&one("system_volume", 80)).expect("80 is in range");
        assert_eq!(update.requires_rearm, vec!["system_volume".to_string()]);
        cleanup(&dir);
    }

    #[test]
    fn an_accepted_level_reaches_the_shared_gains() {
        // Without this the slider would move, the file would save, and
        // nothing the capture session reads would change.
        let (daemon, dir) = writable("applied");
        daemon.set_config(&one("mic_volume", 25)).expect("25 is in range");
        assert_eq!(daemon.gains.mic_percent(), 25);
        cleanup(&dir);
    }

    #[test]
    fn a_refused_change_leaves_the_shared_gains_alone() {
        // config.set is all-or-nothing: a rejected request must not have
        // applied a level the caller was told did not land.
        let (daemon, dir) = writable("refused-gains");
        daemon.set_config(&one("mic_volume", 999)).expect_err("999 is out of range");
        assert_eq!(daemon.gains.mic_percent(), 100);
        cleanup(&dir);
    }
```

- [ ] **Step 2: Run them to verify they fail**

```bash
cargo test -p trix-daemon volume 2>&1 | tail -20
cargo test -p trix-daemon rearm 2>&1 | tail -20
```

Expected: `config.set: unknown key "mic_volume"` from the first, and `no field gains on type Daemon` from the last two.

- [ ] **Step 3: Add the bounds**

In `crates/trix-daemon/src/state.rs`, change the array length and add two entries:

```rust
const NUMERIC_BOUNDS: [(&str, u64, u64); 9] = [
    ("fps", 1, 480),
    ("bitrate_kbps", 1, 200_000),
    ("max_bitrate_kbps", 0, 200_000),
    ("replay_seconds", 1, 600),
    ("monitor_index", 0, 63),
    ("stats_seconds", 0, 86_400),
    ("max_library_gb", 0, 10_000),
    ("system_volume", 0, 100),
    ("mic_volume", 0, 100),
];
```

- [ ] **Step 4: Add the zero-crossing rule**

Add below `REQUIRES_REARM`:

```rust
/// The two capture levels, which follow a different re-arm rule from every
/// other key.
const VOLUME_KEYS: [&str; 2] = ["system_volume", "mic_volume"];

/// True when a level moved onto or off zero.
///
/// Levels apply live: the capture session reads them from a shared cell every
/// pump, so `40 → 70` is audible in the next clip with no re-arm. The
/// exception is zero, which is not a quiet level but a closed capture stream —
/// and streams only open at arm time. Reporting those two transitions, and
/// only those, is what lets the settings page prompt exactly when a prompt
/// changes the outcome.
///
/// A non-numeric value on either side is not a crossing: it is a type error,
/// and `set_config`'s per-key retry produces a far better message for it.
fn crosses_zero(key: &str, before: Option<&Value>, after: Option<&Value>) -> bool {
    if !VOLUME_KEYS.contains(&key) {
        return false;
    }
    let (Some(before), Some(after)) = (before.and_then(Value::as_u64), after.and_then(Value::as_u64))
    else {
        return false;
    };
    (before == 0) != (after == 0)
}
```

In `set_config`'s accepted/re-arm loop, replace the `if REQUIRES_REARM.contains(…)` condition:

```rust
            if REQUIRES_REARM.contains(&key.as_str())
                || crosses_zero(key, base.get(key), after.get(key))
            {
                requires_rearm.push(key.clone());
            }
```

`base` is the pre-merge object already built earlier in the function; do not rebuild it.

- [ ] **Step 5: Give the daemon its gains**

Add the field to `struct Daemon`:

```rust
    /// The capture levels, owned for the daemon's whole lifetime and cloned
    /// into each armed session.
    ///
    /// One instance, not one per arm: `config.set` has to be able to apply a
    /// level whether or not anything is armed, and building a fresh one per
    /// arm would mean it had to find whichever instance the live session
    /// happened to hold — the arrangement where a level change silently lands
    /// on an object nobody is reading.
    pub(crate) gains: Arc<AudioGains>,
```

In `new_at`, before the `Self { … }` literal (the config is moved into the mutex, so read the levels first):

```rust
        let gains = AudioGains::new(config.system_volume, config.mic_volume);
```

and add `gains,` to the struct literal. Add `use trix_core::capture::audio::AudioGains;` to the imports.

In `arm`, replace the Task 5 placeholder:

```rust
        let engine = EngineHandle::spawn(config, Arc::clone(&self.gains))?;
```

In `set_config`, immediately after `*config = updated;`:

```rust
        // After the write, like the hotkey rebind below and for the same
        // reason: a live session capturing at a level the user was told had
        // failed to save is exactly what this ordering prevents.
        self.gains.set(config.system_volume, config.mic_volume);
```

- [ ] **Step 6: Run the tests**

```bash
cargo test -p trix-daemon 2>&1 | tail -10
```

Expected: all seven new tests pass alongside the existing daemon suite.

- [ ] **Step 7: Mutation-check the crossing rule**

Temporarily change `crosses_zero`'s final line to `before != after`. Run `cargo test -p trix-daemon`. Expected: `changing_a_level_within_the_range_needs_no_rearm` fails. Revert and confirm the suite passes.

- [ ] **Step 8: Verify the whole workspace**

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --workspace --all-targets 2>&1 | grep -c "^warning"
cargo fmt --all --check
```

Expected: suite passes, clippy count still 16, `fmt` clean.

- [ ] **Step 9: Commit**

```bash
git add crates/trix-daemon/src/state.rs
git commit -m "feat(daemon): accept and apply the two capture levels"
```

---

### Task 8: UI — slider control and the Audio section

**Files:**
- Modify: `crates/trix-ui/web/src/lib/settings.ts`
- Modify: `crates/trix-ui/web/src/lib/settings.test.ts`
- Modify: `crates/trix-ui/web/src/components/Field.svelte`
- Modify: `crates/trix-ui/web/src/views/Settings.svelte`

**Interfaces:**
- Consumes: the daemon's `system_volume` / `mic_volume` keys and their `0–100` bounds (Task 7).
- Produces: two slider rows in an **Audio** section, placed directly after **Capture**.

All commands in this task run from `crates/trix-ui/web`.

- [ ] **Step 1: Write the failing tests**

In `crates/trix-ui/web/src/lib/settings.test.ts`, extend the shipped-keys list in the `FIELDS` describe block:

```ts
    const shipped = [
      'fps', 'bitrate_kbps', 'max_bitrate_kbps', 'rate_control', 'replay_seconds',
      'monitor_index', 'clip_hotkey', 'gpu_priority', 'stats_seconds', 'clip_dir',
      'max_library_gb', 'autostart', 'system_volume', 'mic_volume',
    ];
```

and add two tests:

```ts
  it('renders both levels as sliders in the Audio section', () => {
    for (const key of ['system_volume', 'mic_volume']) {
      const field = FIELDS.find((f) => f.key === key);
      expect(field, `${key} has no field`).toBeDefined();
      expect(field?.kind).toBe('slider');
      expect(field?.section).toBe('Audio');
    }
  });

  it('bounds both levels at 0 to 100, mirroring the daemon', () => {
    // 100 is unity and the maximum. A page that let someone ask for 150
    // would produce a round trip that fails for a reason the field cannot
    // explain.
    expect(validate('mic_volume', 101)).toMatch(/0 to 100/);
    expect(validate('system_volume', 101)).toMatch(/0 to 100/);
    expect(validate('mic_volume', 0)).toBeNull();
    expect(validate('system_volume', 100)).toBeNull();
  });
```

- [ ] **Step 2: Run them to verify they fail**

```bash
npm test 2>&1 | tail -20
```

Expected: `system_volume has no field`.

- [ ] **Step 3: Extend `settings.ts`**

Add `'slider'` to `FieldKind`:

```ts
export type FieldKind = 'number' | 'text' | 'select' | 'bool' | 'folder' | 'hotkey' | 'slider';
```

Add `'Audio'` to the `section` union in `Field`:

```ts
  section: 'Capture' | 'Audio' | 'Quality' | 'Clips' | 'Trix';
```

Add both keys to `BOUNDS`:

```ts
  system_volume: [0, 100],
  mic_volume: [0, 100],
```

Add both fields to `FIELDS`, directly after the `gpu_priority` entry that ends the Capture group:

```ts
  { key: 'system_volume', label: 'PC sound', kind: 'slider', section: 'Audio', ...span('system_volume'), help: 'How loud your PC\'s own sound is in the clip. Affects the recording only, never your Windows volume. 0 turns it off.' },
  { key: 'mic_volume', label: 'Microphone', kind: 'slider', section: 'Audio', ...span('mic_volume'), help: 'How loud your voice is in the clip. 0 closes the microphone entirely, so Windows stops showing Trix as using it.' },
```

- [ ] **Step 4: Add the slider control**

In `crates/trix-ui/web/src/components/Field.svelte`, add a branch after the `number` branch:

```svelte
    {:else if field.kind === 'slider'}
      <input id={field.key} type="range" min={field.min} max={field.max} step="1"
        value={shown}
        oninput={(e) => (dragging = Number(e.currentTarget.value))}
        onchange={(e) => onset(field.key, Number(e.currentTarget.value))} />
      <span class="hint">{shown}%</span>
```

and add to the `<script>` block, below `$props()`:

```ts
  /**
   * The value under the user's thumb, shown while dragging.
   *
   * The one piece of state this component owns, and it is display-only: the
   * daemon is told on `change` (thumb released), not on `input`, so a drag
   * across the track is one round trip rather than eighty. Without it the
   * percentage beside the slider would sit at the old value for the whole
   * drag, which reads as a broken control.
   */
  let dragging = $state<number | null>(null);
  const shown = $derived(dragging ?? Number(config[field.key] ?? 0));

  // Clear the drag override whenever the authoritative value lands -- whether
  // that is the daemon accepting the change or the parent reloading the config
  // after refusing it. Without this a refused change would leave the slider
  // showing a value the daemon rejected.
  $effect(() => {
    void config[field.key];
    dragging = null;
  });
```

Update the component's doc comment, which currently says it owns no state and describes a six-branch `{#if}`:

```
   * is a seven-branch `{#if}` on `field.kind` (and, for `select`, its
   * `dynamic`) nested inside two `{#each}` blocks -- that reads far better as
   * its own component than inline.
   *
   * Owns one piece of state, `dragging`, documented below. `capture`/
   * `listening`/`heard` live in `Settings.svelte` because the daemon's
```

Add a style rule so the range input is not squeezed by the shared `min-width`:

```css
  input[type='range'] { min-width: 200px; padding: 0; border: none; background: none; }
```

- [ ] **Step 5: Render the section**

In `crates/trix-ui/web/src/views/Settings.svelte`, change the section list:

```svelte
{#each ['Capture', 'Audio', 'Quality', 'Clips', 'Trix'] as section (section)}
```

- [ ] **Step 6: Run the tests and the type check**

```bash
npm test 2>&1 | tail -10
npm run check 2>&1 | tail -10
```

Expected: all frontend tests pass (48 existing + 2 new), `svelte-check` reports 0 errors.

- [ ] **Step 7: Verify UI isolation**

```bash
powershell -ExecutionPolicy Bypass -File ../../../scripts/ui-isolation.ps1
```

Expected: OK. `trix-ui` must not have gained a `trix-core` dependency.

- [ ] **Step 8: Commit**

```bash
cd ../../..
git add crates/trix-ui
git commit -m "feat(ui): PC sound and microphone level sliders"
```

---

## Final Verification

Run every gate the branch has to clear, from the repo root.

- [ ] **Step 1: The full machine gate**

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --workspace --all-targets 2>&1 | grep -c "^warning"
cargo fmt --all --check
```

Expected: all tests pass; clippy count still 16 (12 real warnings plus 4 summary lines, all pre-existing) with none in the new code; `fmt` clean.

```bash
cd crates/trix-ui/web && npm test 2>&1 | tail -5 && npm run check 2>&1 | tail -5 && cd ../../..
powershell -ExecutionPolicy Bypass -File scripts/ui-isolation.ps1
powershell -ExecutionPolicy Bypass -File scripts/ui-smoke.ps1
```

Expected: frontend tests pass, `svelte-check` 0 errors, isolation OK, `ui-smoke` 9/9.

- [ ] **Step 2: Hand verification (requires the user)**

These are the checks no machine on this branch can make. Present them to the user as a list; do not claim any of them passed without their word.

1. With both levels at `100`, record a clip while talking. Voice and game audio are both audible and in sync with the video.
2. Set **Microphone** to `0`. While armed, the Windows taskbar shows **no** microphone-in-use indicator for Trix. A clip taken now has game audio only.
3. Set **Microphone** back to `50` while armed. The settings page shows the "Re-arm to apply" banner (this crossed zero).
4. Re-arm, then drag **Microphone** from `50` to `80` while armed. **No** banner appears, and the next clip is louder.
5. Set both levels to `0` and take a clip. The MP4 has no audio track at all.
6. Unplug the microphone (or disable it in Windows) and arm. Arming succeeds and system audio still records.

- [ ] **Step 3: Release notes**

The spec makes this a requirement, not a suggestion (§3.1): the microphone is on by default at `100`, so everyone who updates begins recording their voice without being asked. Whatever text accompanies the release must lead with that.

---

## Notes for the executor

- **Tasks 3 through 5 leave the workspace uncompilable in between.** That is deliberate — the source rename, the mixer, and the consumer rewiring are three separately reviewable changes. Commit each anyway; do not merge them to keep `cargo build` green at every commit.
- **Do not touch the first-run wizard.** The user asked for this explicitly. The sliders live in Settings only.
- **`AudioTimeline` is not modified** except for the `log_diagnostics` label in Task 4. If a change to it seems necessary, that is a signal the mixer is doing something the timeline should — stop and ask.
