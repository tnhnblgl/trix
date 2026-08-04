# Trix — Microphone Capture & Audio Levels

**Date:** 2026-08-04
**Status:** Approved design, not yet implemented
**Extends** [2026-07-26-trix-desktop-ui-design.md](2026-07-26-trix-desktop-ui-design.md) (the daemon and
desktop app, shipped as v0.3.0) and PLAN.md's Phase 3–4 audio path.

---

## 1. Context

Trix records system audio today. `LoopbackCapture` opens an event-driven WASAPI shared-mode loopback
stream on the default render device, and `AudioTimeline` places the resulting packets on the video's
QPC clock — trimming overlap, filling idle gaps with synthesized silence, and absorbing sub-20 ms
timestamp jitter without splicing. Both the replay ring and the direct-record path consume that one
continuous PCM stream.

What it cannot do is record the user's voice, and it offers no control over how loud either source is
in the finished clip. For a clip tool aimed at people who want to share plays, a commentary track is
not a nice-to-have — it is most of why anyone watches the clip.

Two things make this cheaper than it looks. `AudioTimeline` was written source-agnostic: it takes a
receiver and a sink and knows nothing about where packets came from. And a microphone is the same
WASAPI shared-mode capture the loopback path already performs, differing only in the device it opens
and the absence of the loopback flag.

## 2. Scope

**In scope:**

- Microphone capture from the Windows default input device
- Mixing microphone and system audio into the single audio track clips already carry
- Two config keys, `system_volume` and `mic_volume`, both `0–100`, both defaulting to `100`
- Two sliders in a new **Audio** section of the settings page
- Live gain changes that do not tear down the replay ring

**Explicitly out of scope** (each is its own change, and none of this design forecloses them):

- A microphone picker. Trix follows the Windows default input device. Plug in a headset and Trix
  follows it with no settings visit, at the cost of not being able to record the second of two
  connected mics. Adding a picker later needs a device-enumeration command and a persisted device ID;
  nothing here has to be redone for it.
- Separate microphone and system audio tracks in the MP4. Multi-stream muxing only pays off once
  there is an editor to use it, which is `library.export`, the next plan.
- Gain above unity. `100` is unity and the maximum, so Trix can never be the reason a clip clips.
  A quiet microphone is raised in Windows' own input settings.
- Push-to-talk, noise suppression, ducking, per-application audio capture.
- Any change to the first-run wizard. The sliders live in Settings only.

## 3. Product behaviour

### 3.1 The two levels

| Key | Type | Default | Bounds | Controls |
|---|---|---|---|---|
| `system_volume` | `u32` | `100` | `0–100` | how loud the PC's own sound is in the clip |
| `mic_volume` | `u32` | `100` | `0–100` | how loud the user's voice is in the clip |

Both control **only what is written into the clip**. Neither touches the Windows volume mixer, the
device's own level, or anything else outside Trix's own process.

**The microphone is on by default at `100`.** This was decided explicitly with the alternative in
view: it means a user who updates to this version starts recording their voice without being asked
once. Two consequences follow and are requirements, not suggestions:

- The release notes for the version carrying this change must lead with it.
- The Audio section must be visible in Settings without scrolling past unrelated fields, so that a
  user who opens Settings for any reason meets the control rather than discovering it in a clip.

### 3.2 Zero means closed, not muted

At `0`, the source's capture stream is **never opened** — not opened and multiplied by zero.

This is a deliberate product requirement rather than an optimisation. Windows 11 shows a microphone
indicator in the taskbar whenever a process holds an input stream open. A recorder that keeps the
microphone open while its own slider reads `0` is indistinguishable, from the outside, from one that
is lying about it.

With both keys at `0`, the clip's MP4 carries **no audio stream at all**, rather than a track of
silence.

### 3.3 The gain curve

Gain is the percentage squared:

```
gain = (percent / 100)²
```

So `100` → `1.0` (unity, bit-for-bit unchanged), `50` → `0.25`, `0` → `0.0`.

Scaling amplitude directly by the percentage makes a slider feel dead: because loudness is
roughly logarithmic in amplitude, every audible change crowds into the bottom third of the travel and
the top half does almost nothing. Squaring is the standard fader taper and puts the useful range
under the user's thumb. `50` is therefore about half as *loud*, not half the amplitude.

### 3.4 Live changes, and the one exception

Changing either level while armed takes effect **immediately**, without re-arming.

This is not a convenience. Re-arming destroys the replay ring, and with it the last fifteen seconds
the user may be about to clip — the reason `REQUIRES_REARM` exists and is kept short. A volume
slider that costs the user their buffer on every nudge is a broken volume slider, and volume is the
one setting people adjust by trial while the thing they are adjusting is running.

**The exception is crossing zero.** Moving a level off `0`, or onto `0`, starts or stops a real
capture stream, which only happens at arm time. Those two transitions report the key in
`config.set`'s existing `requires_rearm` list, and the settings page's existing "Re-arm to apply"
banner handles it with no new UI.

Changes within `1–100` never report `requires_rearm`.

### 3.5 When audio fails

Audio never fails an arm. If the microphone cannot be opened — none plugged in, held exclusively by
another application, driver refusal — Trix logs a warning and records with system audio alone. This
is byte-for-byte how the loopback path already behaves when the render device is unavailable, and
the same fallback now applies independently to each of the two sources.

A microphone that disappears mid-session (unplugged headset) stops producing packets. The timeline's
existing silence filler covers the gap, so the clip carries silence on the voice track for the
remainder rather than desynchronising or truncating.

Mono microphones, 44.1 kHz microphones, and headset microphones all work without special handling:
WASAPI shared mode with `autoconvert: true` resamples and up-mixes to the 48 kHz interleaved stereo
i16 the encoder expects — the same mechanism the loopback path already relies on.

## 4. Architecture

```
system audio ──→ AudioTimeline ──┐
                                 ├──→ AudioMixer ──→ one PCM stream ──→ ring / AAC encoder
microphone   ──→ AudioTimeline ──┘
```

Both timelines are anchored to the same instant — the first video frame's QPC stamp — and both are
pumped to the same target. Because each emits a gapless stream from frame 0, silence-filling
whatever the device did not supply, the two are frame-aligned by construction and mix by addition.

**`AudioMixer` presents the same surface `AudioTimeline` does today**, so nothing downstream changes:
the replay ring, its eviction, the clip snapshot, and the MF muxer are untouched. `replay.rs` and
`record.rs` each swap a `(Option<Receiver<AudioPacket>>, AudioTimeline)` pair for one `AudioMixer`,
and their pump call sites lose an argument.

**The mixer owns the capture threads too.** Today `replay.rs` and `record.rs` each carry a separate
`audio_handle: Option<LoopbackCapture>` alongside the receiver, and shut it down by hand. With two
sources that becomes two handles and two shutdowns in each of two files — four chances to leak a
capture thread, on the code path a memory leak was already found and fixed in once. `AudioMixer::stop`
owns both, so each call site keeps exactly one thing to hold and one thing to stop.

### 4.1 Mixing at capture time, not at clip time

The mixer produces one stream, and the replay ring stores that one stream. The alternative — two
rings mixed when a clip is saved — would let a level change apply retroactively to audio already
buffered.

One ring wins on the thing this project exists for. A second PCM ring costs 192 KB/s of RAM
(48 kHz × 2 channels × 2 bytes), or about 2.9 MB at the default 15-second buffer, on a tool whose
entire reason to exist is not spending the user's machine. It would also require duplicating the
ring's eviction and snapshot logic for a second stream — the code most likely to produce an
out-of-sync clip if the two copies ever drift.

The cost is that a level change applies only to audio captured after it, so the seconds already in
the buffer keep the old levels. Within a 15-second buffer this self-corrects in 15 seconds.

Mixing costs two multiply-adds per sample at 96 000 samples/second. It does not register.

### 4.2 Clipping

Samples are summed in a wider integer and clamped to `i16` range. A loud game plus a loud voice
distorts, which is unpleasant; wrapping around would invert the waveform and produce a scream, which
is unusable. Since neither gain can exceed unity, clipping requires both sources to be genuinely
loud at once.

### 4.3 Files

`crates/trix-core/src/capture/audio.rs` is 411 lines and this adds roughly 250. It becomes a
directory of focused files:

| File | Responsibility |
|---|---|
| `capture/audio/mod.rs` | Shared constants (`SAMPLE_RATE`, `CHANNELS`, `ENCODER_BLOCK_ALIGN`, the grace and dead-band windows), `AudioPacket`, `frames_to_100ns` / `dur_100ns_to_frames`, re-exports |
| `capture/audio/source.rs` | WASAPI session setup and the capture thread, for both the render-device loopback and the input-device microphone |
| `capture/audio/timeline.rs` | `AudioTimeline`, moved verbatim |
| `capture/audio/mixer.rs` | `AudioGains`, `AudioMixer`, and the pure gain/mix functions |

`record_wav` (probe mode) stays loopback-only and moves with the source code unchanged.

### 4.4 Interfaces

`AudioGains` — the live levels, shared between the daemon's config and the running capture session:

```rust
/// Per-source capture gains, as whole percentages. Shared between the daemon
/// (which writes on `config.set`) and the capture session (which reads each pump).
pub struct AudioGains { /* two AtomicU32 */ }

impl AudioGains {
    pub fn new(system_percent: u32, mic_percent: u32) -> Arc<Self>;
    /// Applied to audio captured from here on; already-buffered audio keeps its levels.
    pub fn set(&self, system_percent: u32, mic_percent: u32);
    /// Linear multipliers, i.e. (percent / 100)².
    pub fn system(&self) -> f32;
    pub fn mic(&self) -> f32;
}
```

The pure functions, which are where the real unit tests live:

```rust
/// Scales interleaved i16 PCM in place. A gain of exactly 1.0 returns untouched.
pub fn apply_gain(pcm: &mut [u8], gain: f32);

/// Adds `src` scaled by `gain` into `dst`, sample-wise, clamping to i16 range.
/// Mixes `min(dst.len(), src.len())` bytes and leaves any excess of `dst` alone.
pub fn mix_into(dst: &mut [u8], src: &[u8], gain: f32);
```

`AudioMixer` — mirrors the surface `AudioTimeline` exposes today:

```rust
impl AudioMixer {
    /// Opens whichever sources have a non-zero level. Never fails: a source that
    /// cannot be opened is logged and omitted.
    pub fn start_sources(gains: Arc<AudioGains>) -> Self;
    /// The same mixer over receivers the caller supplies, which `start_sources`
    /// wraps. This is the seam §8's mixer tests drive with synthetic packets —
    /// without it, every mixing test would need a real microphone attached.
    pub fn with_sources(
        system: Option<Receiver<AudioPacket>>,
        mic: Option<Receiver<AudioPacket>>,
        gains: Arc<AudioGains>,
    ) -> Self;
    /// True when at least one source opened — this is `RecorderSettings::with_audio`.
    pub fn active(&self) -> bool;
    /// Anchors both timelines; the first call wins.
    pub fn start(&mut self, t0_qpc: i64);
    pub fn started(&self) -> bool;
    /// Emits gapless consecutive mixed chunks as `(start_frame, pcm)`.
    pub fn pump(&mut self, target_qpc: i64, sink: &mut impl FnMut(u64, &[u8]));
    pub fn frames_emitted(&self) -> u64;
    /// Logs each open source's splice diagnostics, tagged by source.
    pub fn log_diagnostics(&self);
    /// Stops the capture threads.
    pub fn stop(self) -> Result<()>;
}
```

`pump` drains each open timeline into its own staging buffer, mixes the common prefix
(`n = min(len)`) so a source that is momentarily short cannot shift the other's alignment, emits `n`
bytes, and retains the remainder for the next call. With one open source the mix step is skipped and
only `apply_gain` runs; with a level of exactly `100` that too is a no-op, making the single-source
default path bit-for-bit identical to today's output.

## 5. Daemon and protocol

No new protocol commands. The two keys ride the existing `config.get` / `config.set` surface, whose
known-key set is derived from a serialized `Config::default()` and therefore picks them up with no
change to the dispatcher.

**`state.rs`:**

- `NUMERIC_BOUNDS` grows two entries: `("system_volume", 0, 100)` and `("mic_volume", 0, 100)`.
- `REQUIRES_REARM` stays as it is — it is a list of keys that *always* require a re-arm, and neither
  volume key does. The zero-crossing rule of §3.4 is a separate check in `set_config`, comparing the
  before and after values it already has in hand, and it appends to the same `requires_rearm` list
  the response carries.
- `set_config` pushes the new levels into the shared `AudioGains` after the file write succeeds, in
  the same place and for the same reason `clip_hotkey` rebinds there: a live session that disagreed
  with a config the user was told had failed to save is the bug that ordering prevents.

**The daemon owns exactly one `AudioGains` for its whole lifetime**, built from the config at
startup. `set_config` updates that one; arming clones the `Arc` into the session. Building a fresh
one per arm would mean `set_config` had to find and update whichever instance the live session
happened to hold — the arrangement where a level change silently lands on an object nobody is
reading. One owner makes "the levels the daemon holds" and "the levels the session reads" the same
thing by construction, whether or not anything is armed.

## 6. CLI

`trix record --no-audio` keeps its meaning and gets stronger: no audio at all, both sources off,
no audio stream in the MP4. Without the flag, `record` and `replay` both honour the config levels.

No new CLI flags. A user who wants to record without a microphone sets `mic_volume` to `0`, which is
the same control the app uses and the same one that survives a restart.

## 7. UI

`crates/trix-ui/web/src/lib/settings.ts`:

- `FieldKind` gains `'slider'`.
- The `section` union gains `'Audio'`.
- `BOUNDS` gains both keys, mirroring the daemon's, as the existing comment there requires.
- Two `FIELDS` entries in section `Audio`.

`crates/trix-ui/web/src/components/Field.svelte` gains a seventh branch on `field.kind` rendering a
range input with its current percentage shown beside it, so the value is readable without dragging.

`crates/trix-ui/web/src/views/Settings.svelte`'s hardcoded section list becomes
`['Capture', 'Audio', 'Quality', 'Clips', 'Trix']` — Audio directly after Capture, satisfying §3.1's
visibility requirement.

Help text states plainly that the levels affect the recording only and that `0` turns the source off.

**`trix-ui` still must not depend on `trix-core`** (spec §3.2 of the desktop design, enforced by
`scripts/ui-isolation.ps1`). Nothing here needs it to.

## 8. Testing

**Unit tests, no hardware required** — the pure functions carry the risk and take the coverage:

- `apply_gain` at `1.0` leaves the buffer byte-for-byte unchanged.
- `apply_gain` at `0.0` produces silence.
- `apply_gain` at a midpoint scales sample values, not bytes (a sanity check that the i16 framing is
  respected and the buffer is not being treated as `u8`).
- `mix_into` sums two buffers sample-wise.
- `mix_into` clamps rather than wraps: two near-full-scale positive buffers produce `i16::MAX`, two
  near-full-scale negative buffers produce `i16::MIN`.
- `mix_into` with mismatched lengths mixes the common prefix and leaves the excess of `dst` intact.
- `AudioGains` maps `100 → 1.0`, `0 → 0.0`, and is monotonic between.
- `AudioMixer::pump` with two synthetic sources emits frame-aligned mixed output; with one source it
  emits that source's stream scaled; with none it emits nothing and reports `active() == false`.
- `set_config` reports `requires_rearm` for a level crossing zero in either direction, and does not
  report it for a change within `1–100`.
- Out-of-range levels are refused by `check_ranges` with the existing message.

**Hand verification** — the parts only ears can settle:

- A clip recorded with both at `100` has audible voice and game audio, in sync with the video.
- `mic_volume = 0` produces a clip with game audio only, **and** no microphone indicator in the
  Windows taskbar while armed (§3.2's actual claim).
- Dragging a level between `1` and `100` while armed changes subsequent clips without a re-arm
  prompt and without dropping the buffer.
- Moving a level onto or off `0` while armed shows the "Re-arm to apply" banner.
- Arming with no microphone connected succeeds and records system audio.

**Isolation rules from the desktop plan still bind:** any test that starts a real daemon sets both a
scratch `%APPDATA%` and a scratch `clip_dir`, and any test reaching daemon process globals accounts
for the fact that `cargo test` runs the whole binary's tests as threads in one process.

## 9. Risks

| Risk | Handling |
|---|---|
| Users are recorded without realising | Accepted deliberately (§3.1). Mitigated by release notes and by Audio sitting second in Settings. |
| Microphone drift against the render device's clock | Each source has its own `AudioTimeline`, which re-anchors by QPC stamp and fills or trims. This is the mechanism that already absorbs loopback's own drift. |
| Squared taper feels wrong in the ear | Isolated to `AudioGains`' two accessors; changing the curve is a one-line change with its own unit test. |
| Splitting `audio.rs` obscures a regression in moved code | `AudioTimeline` moves verbatim in its own commit, with its existing tests moving with it, so the move and the new behaviour are separable in review. |

## 10. Definition of done

- Both levels present in `config.toml`, `config.get`, `config.set`, and the settings page.
- A clip carries mixed microphone and system audio at the configured levels.
- `mic_volume = 0` leaves the microphone closed, verified against the Windows taskbar indicator.
- Both at `0` produces a clip with no audio stream.
- Levels change live between `1` and `100`; only zero crossings prompt a re-arm.
- Arming with no microphone succeeds.
- Workspace tests, frontend tests, `svelte-check`, `cargo fmt`, `cargo clippy`,
  `scripts/ui-isolation.ps1`, and `scripts/ui-smoke.ps1` all as clean as they are at v0.3.0.
