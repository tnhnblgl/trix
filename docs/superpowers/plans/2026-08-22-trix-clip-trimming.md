# Clip Trimming (fast mode) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user set in/out points on a saved clip and export the trimmed range as a new clip in the library, losslessly and in under a second.

**Architecture:** Fast mode only — a stream copy. An `IMFSourceReader` hands over the source MP4's already-encoded H.264 and AAC samples and an `IMFSinkWriter` writes them straight back out, so no decoder and no encoder is involved. Trim points snap to keyframes, which the 1-second GOP pin (already shipped) caps at ~1s of error. The export runs in the daemon, synchronously, and answers with the new clip's `ClipMeta` exactly the way `clip` does.

**Tech Stack:** Rust 2024, `windows` 0.62 (Media Foundation), Svelte 5 runes, Tauri v2.

## Global Constraints

- 100% Rust. No ffmpeg, no new media dependencies. Media Foundation only, through the existing `windows` crate.
- Windows-only. No cross-platform abstraction.
- **Fast mode only.** Precise (re-encode) mode is explicitly out of scope and is a later plan. Any `mode` value other than `"fast"` is refused with a clear error.
- The source clip is never modified, moved, or deleted by an export. Trim is non-destructive (spec §6.2).
- The daemon owns Media Foundation. The UI never touches it.
- Tests must never read or write the developer's real `%APPDATA%\trix\config.toml` or real clip library. Use a scratch dir, following `dispatch.rs`'s `idle()` / `request_with()` helpers.
- Commits carry no `Co-Authored-By` trailer and no "Generated with" line. Author stays `tnhnblgl <tnhnblgl@gmail.com>`.
- Do not push. The user pushes.
- Workspace version stays `0.6.0` — this plan does not bump it.

## Deviations from spec §6.2 / §6.3 — read before starting

The spec was written before the daemon existed in its current shape, and the user settled three scoping questions on 2026-08-22. These deviations follow from those answers. **They are recorded here, not hidden:** if any is wrong, say so before Task 1 rather than after Task 6.

| Spec says | This plan does | Why |
|---|---|---|
| `library.export` takes `{clip_id, dest, start_ms, end_ms, mode}` | `{clip_id, start_ms, end_ms, mode}` — no `dest` | The export lands as a new clip in the library, so the daemon allocates the id and the path. A caller-supplied `dest` would put the file somewhere the grid can never show it. |
| `export_progress` / `export_done` events | Neither. The response carries the new `ClipMeta`, and `clip_saved` broadcasts it | A fast-mode export is sub-second and synchronous, like `clip`. Progress events for an operation that finishes before a UI can paint a bar are noise. Precise mode is what needs them; it can add them. |
| A `trim_mode` config key with a segmented control | Not added | A setting with one legal value is not a setting. `library.export` still takes `mode` on the wire, so the protocol shape is already right for precise mode to slot into. |
| "filmstrip trim bar" | A scrub bar with in/out handles and keyframe ticks — no decoded thumbnails | User's call, 2026-08-22. The daemon decodes no video today; a thumbnail strip is its own sub-feature. |

**One protocol addition the spec does not mention:** §6.3 requires the bar to draw "faint tick marks at every keyframe", but no command exposes keyframe positions. This plan adds `library.keyframes`.

## File Structure

**Create:**
- `crates/trix-core/src/export.rs` — the whole engine. Reads compressed samples from an MP4, indexes keyframes, and remuxes a range. One responsibility: turning an MP4 plus a time range into a new MP4 without an encoder.
- `crates/trix-ui/web/src/components/TrimBar.svelte` — the scrub bar, in/out handles, keyframe ticks. Presentational: it takes duration, playhead, keyframes, and in/out values, and emits changes. It knows nothing about the daemon.

**Modify:**
- `crates/trix-core/src/lib.rs` — register the module.
- `crates/trix-proto/src/command.rs` — two new `Command` variants and their parsing.
- `crates/trix-daemon/src/state.rs` — `Daemon::keyframes` and `Daemon::export_clip`.
- `crates/trix-daemon/src/dispatch.rs` — two new match arms.
- `crates/trix-ui/web/src/lib/state.svelte.ts` — `app.keyframes(id)` and `app.exportTrim(...)`.
- `crates/trix-ui/web/src/views/ClipPage.svelte` — mount the bar in the slot already reserved for it, add `I` / `O` / `Ctrl+E`.
- `crates/trix-ui/web/src/lib/keys.ts` — only if `shouldHandleKey` needs to let the new keys through.
- `docs/ship/README.txt` — the "No trimming or exporting yet" line becomes untrue.

---

### Task 1: Keyframe index and range math

**Files:**
- Create: `crates/trix-core/src/export.rs`
- Modify: `crates/trix-core/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `crates/trix-core/src/export.rs`

**Interfaces:**
- Consumes: `trix_core::encode::mf::ensure_mf_started` (already `pub`).
- Produces:
  - `pub fn keyframes_ms(source: &Path) -> anyhow::Result<Vec<u64>>` — ascending, always starts with 0.
  - `pub fn snap_start(keyframes_ms: &[u64], start_ms: u64) -> u64`
  - `pub fn clamp_range(duration_ms: u64, start_ms: u64, end_ms: u64) -> Result<(u64, u64), String>`
  - `pub const MIN_TRIM_MS: u64 = 200;`

**Why the pure functions come first:** they are the part a test can pin down without a GPU, a codec, or a real file. The Media Foundation half gets an `#[ignore]`d test that runs against a real clip by hand.

- [ ] **Step 1: Write the failing tests**

Create `crates/trix-core/src/export.rs` with only the tests and the signatures:

```rust
//! Fast-mode clip export: a keyframe-snapped stream copy of an existing MP4.
//!
//! Spec §6.3's `fast` mode. No decoder and no encoder is involved: an
//! `IMFSourceReader` hands over the source's already-encoded H.264 and AAC
//! samples and an `IMFSinkWriter` writes them straight back out. That is what
//! makes an export sub-second and lossless, and it is also why the in-point can
//! only land on a keyframe -- nothing in this pipeline could synthesize the
//! frames between one keyframe and the next.

use std::path::Path;

use anyhow::Result;

/// Shortest export worth writing. Below this the output is a file with one or
/// two frames in it, which is a mistake rather than a clip.
pub const MIN_TRIM_MS: u64 = 200;

/// Latest keyframe at or before `start_ms`.
///
/// Snapping *backwards* rather than to the nearest keyframe is deliberate: a
/// forward snap silently drops footage the user explicitly asked to keep, and
/// the frame they set the in-point on is usually the one that matters.
pub fn snap_start(keyframes_ms: &[u64], start_ms: u64) -> u64 {
    keyframes_ms.iter().copied().filter(|&k| k <= start_ms).next_back().unwrap_or(0)
}

/// Orders and clamps a requested range against the clip's real duration.
///
/// `Err` is the text of the error response, so it says what was wrong in terms
/// the person driving the socket can act on.
pub fn clamp_range(duration_ms: u64, start_ms: u64, end_ms: u64) -> Result<(u64, u64), String> {
    if duration_ms == 0 {
        return Err("this clip has no duration to trim".into());
    }
    if end_ms <= start_ms {
        return Err(format!("end_ms ({end_ms}) must be greater than start_ms ({start_ms})"));
    }
    let start = start_ms.min(duration_ms);
    let end = end_ms.min(duration_ms);
    if end.saturating_sub(start) < MIN_TRIM_MS {
        return Err(format!("a trim must be at least {MIN_TRIM_MS} ms long"));
    }
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_start_goes_back_to_the_previous_keyframe() {
        let keys = [0u64, 1000, 2000, 3000];
        assert_eq!(snap_start(&keys, 2500), 2000, "snaps back, never forward");
        assert_eq!(snap_start(&keys, 2000), 2000, "an exact keyframe stays put");
        assert_eq!(snap_start(&keys, 0), 0);
    }

    /// A clip whose index somehow arrives without a leading 0 must still export
    /// from the top rather than refuse.
    #[test]
    fn snap_start_falls_back_to_zero_when_nothing_is_early_enough() {
        assert_eq!(snap_start(&[5000, 6000], 1000), 0);
        assert_eq!(snap_start(&[], 1000), 0);
    }

    #[test]
    fn clamp_range_refuses_inverted_and_tiny_ranges() {
        assert!(clamp_range(10_000, 5_000, 5_000).is_err(), "zero-length");
        assert!(clamp_range(10_000, 5_000, 4_000).is_err(), "inverted");
        assert!(clamp_range(10_000, 0, MIN_TRIM_MS - 1).is_err(), "below the floor");
        assert!(clamp_range(0, 0, 1_000).is_err(), "a clip with no duration");
    }

    #[test]
    fn clamp_range_trims_an_overlong_end_to_the_clip() {
        assert_eq!(clamp_range(10_000, 8_000, 99_000), Ok((8_000, 10_000)));
    }
}
```

Register it in `crates/trix-core/src/lib.rs` alongside the other `pub mod` lines:

```rust
pub mod export;
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p trix-core export::`

Expected: 4 passed. (These are written complete rather than failing-first because they are pure assertions over functions small enough to read — the failing-first cycle belongs to `keyframes_ms` below, which cannot be unit-tested at all.)

- [ ] **Step 3: Add the compressed-sample reader and the keyframe index**

Append to `crates/trix-core/src/export.rs`, above the test module:

```rust
use windows::Win32::Media::MediaFoundation::{
    IMFSample, IMFSourceReader, MFCreateSourceReaderFromURL, MFSampleExtension_CleanPoint,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::core::HSTRING;

use crate::encode::mf::ensure_mf_started;

/// A read that returns no sample without being the end of the stream is legal
/// (a format change, a gap). Skipping is correct; treating it as the end would
/// truncate. Bounded so a source that only ever returns nothing cannot spin
/// forever -- same reasoning, and the same number, as `sound/decode.rs`.
const MAX_CONSECUTIVE_EMPTY_READS: u32 = 64;

fn video_stream() -> u32 {
    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32
}

fn audio_stream() -> u32 {
    MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32
}

/// Opens `source` for **compressed** reading.
///
/// The load-bearing detail is what this does *not* do: it never calls
/// `SetCurrentMediaType`. Ask a source reader for a decoded type and it inserts
/// a decoder; leave the type alone and it hands over the file's own H.264 and
/// AAC samples untouched. That is the whole stream copy.
fn open_compressed(source: &Path, with_audio: bool) -> Result<IMFSourceReader> {
    if !source.is_file() {
        anyhow::bail!("{} is not a file", source.display());
    }
    ensure_mf_started()?;
    let reader: IMFSourceReader =
        unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(source), None) }
            .with_context(|| format!("Windows could not open {}", source.display()))?;
    unsafe {
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
            .context("deselecting every stream")?;
        reader.SetStreamSelection(video_stream(), true).context("selecting the video stream")?;
        if with_audio {
            // A clip recorded with audio off has no audio stream at all, and
            // that is not an error -- it is a silent clip.
            let _ = reader.SetStreamSelection(audio_stream(), true);
        }
    }
    Ok(reader)
}

/// One compressed sample's timing, without the payload.
struct SampleTiming {
    pts_100ns: i64,
    duration_100ns: i64,
    keyframe: bool,
}

fn timing(sample: &IMFSample) -> SampleTiming {
    unsafe {
        SampleTiming {
            pts_100ns: sample.GetSampleTime().unwrap_or(0),
            duration_100ns: sample.GetSampleDuration().unwrap_or(0),
            // Absent means "not a clean point". An error here is the attribute
            // being missing, which is the common case for a P-frame.
            keyframe: sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 1,
        }
    }
}

/// Every keyframe position in `source`, in milliseconds, ascending.
///
/// Always starts with 0: the first sample of a Trix clip is a keyframe by
/// construction (`replay.rs` refuses to mux a snapshot whose first packet is
/// not one), and a UI that draws ticks needs a tick at the start regardless.
///
/// This is a full demux pass with no decoding, which on a replay-buffer-length
/// clip is tens of milliseconds. It is not cached: the UI asks once per clip
/// opened, and a cache would have to be invalidated on delete and on a clip_dir
/// move for no measurable gain.
pub fn keyframes_ms(source: &Path) -> Result<Vec<u64>> {
    let reader = open_compressed(source, false)?;
    let stream = video_stream();
    let mut keys = Vec::new();
    let mut empty_reads = 0u32;
    loop {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        unsafe { reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample)) }
            .with_context(|| format!("reading video from {} failed", source.display()))?;
        if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
            break;
        }
        let Some(sample) = sample else {
            empty_reads += 1;
            if empty_reads > MAX_CONSECUTIVE_EMPTY_READS {
                anyhow::bail!("{} stopped delivering video samples", source.display());
            }
            continue;
        };
        empty_reads = 0;
        let t = timing(&sample);
        if t.keyframe {
            keys.push((t.pts_100ns.max(0) / 10_000) as u64);
        }
    }
    if keys.first() != Some(&0) {
        keys.insert(0, 0);
    }
    Ok(keys)
}
```

Add `Context as _` to the `anyhow` import at the top of the file:

```rust
use anyhow::{Context as _, Result};
```

- [ ] **Step 4: Add the by-hand integration test**

Append inside `mod tests`:

```rust
    /// Runs against a real clip, so it is `#[ignore]`d: `cargo test` on a
    /// machine with no clips must not fail, and this needs Media Foundation,
    /// a real H.264 file, and a path only the developer knows.
    ///
    /// Run it with:
    ///   cargo test -p trix-core -- --ignored keyframe_index_of_a_real_clip
    /// after setting TRIX_TEST_CLIP to the full path of an .mp4 in your library.
    #[test]
    #[ignore = "needs a real clip; set TRIX_TEST_CLIP"]
    fn keyframe_index_of_a_real_clip() {
        let Ok(path) = std::env::var("TRIX_TEST_CLIP") else {
            panic!("set TRIX_TEST_CLIP to an .mp4 path");
        };
        let keys = keyframes_ms(Path::new(&path)).expect("indexing a real clip");
        assert_eq!(keys.first(), Some(&0), "a clip always has a keyframe at 0");
        assert!(keys.len() > 1, "a GOP-pinned clip has one keyframe per second: {keys:?}");
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "ascending and unique: {keys:?}");
    }
```

- [ ] **Step 5: Verify it builds and the pure tests still pass**

Run: `cargo test -p trix-core export::`

Expected: 4 passed, 1 ignored.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-core/src/export.rs crates/trix-core/src/lib.rs
git commit -m "feat(export): index a clip's keyframes without decoding"
```

---

### Task 2: The fast-mode remux

**Files:**
- Modify: `crates/trix-core/src/export.rs`
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Consumes: `open_compressed`, `timing`, `snap_start`, `clamp_range`, `video_stream`, `audio_stream` from Task 1.
- Produces:
  - `pub struct FastExport<'a> { pub source: &'a Path, pub dest: &'a Path, pub start_ms: u64, pub end_ms: u64 }`
  - `pub struct ExportOutcome { pub duration_ms: u64, pub video_packets: usize, pub has_audio: bool }`
  - `pub fn export_fast(request: FastExport<'_>) -> anyhow::Result<ExportOutcome>`

**Two design points the implementer must not "improve":**

1. **No seeking.** `IMFSourceReader::SetCurrentPosition` is gated behind the `Win32_System_Com_StructuredStorage` cargo feature, which this workspace does not enable, and enabling it drags `PROPVARIANT` in for one call. Instead the reader runs from the start and samples before the snapped in-point are read and dropped. On a replay-buffer-length clip that is a demux with no decode — cheap. If a future precise mode needs real seeking, that is when the feature gets added.
2. **Video first, then audio, in two passes.** The sink writer is created with `MF_SINK_WRITER_DISABLE_THROTTLING`, exactly as `ClipMuxer` does and for the same documented reason: a throttled writer blocks `WriteSample` waiting for the lagging stream. Writing one whole track and then the other is the pattern that already works in this codebase.

- [ ] **Step 1: Write the failing test**

Append inside `mod tests`:

```rust
    /// The other half of the by-hand gate. Exports the middle of a real clip
    /// and asserts the output is a real, shorter, independently indexable MP4.
    ///
    ///   cargo test -p trix-core -- --ignored fast_export_of_a_real_clip
    #[test]
    #[ignore = "needs a real clip; set TRIX_TEST_CLIP"]
    fn fast_export_of_a_real_clip() {
        let Ok(path) = std::env::var("TRIX_TEST_CLIP") else {
            panic!("set TRIX_TEST_CLIP to an .mp4 path");
        };
        let source = Path::new(&path);
        let dest = std::env::temp_dir().join("trix-export-test.mp4");
        let _ = std::fs::remove_file(&dest);

        let outcome = export_fast(FastExport { source, dest: &dest, start_ms: 1_000, end_ms: 3_000 })
            .expect("exporting a real clip");

        assert!(outcome.video_packets > 0, "an export with no video is a failure");
        assert!(dest.is_file(), "the output must exist");
        assert!(
            std::fs::metadata(&dest).unwrap().len() < std::fs::metadata(source).unwrap().len(),
            "a 2 s trim of a longer clip must be smaller than the source"
        );
        // The proof it is a valid MP4 and not just bytes: re-index it.
        let keys = keyframes_ms(&dest).expect("the export must itself be readable");
        assert_eq!(keys.first(), Some(&0), "the export must start on a keyframe");

        let _ = std::fs::remove_file(&dest);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p trix-core -- --ignored fast_export_of_a_real_clip`

Expected: FAIL to compile — `cannot find function export_fast in this scope`.

- [ ] **Step 3: Implement the remux**

Append to `crates/trix-core/src/export.rs`, above the test module. Extend the Media Foundation import list with the sink-writer names:

```rust
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSinkWriter, MFCreateAttributes, MFCreateSinkWriterFromURL,
    MF_SINK_WRITER_DISABLE_THROTTLING,
};
```

```rust
/// What to export, and to where.
pub struct FastExport<'a> {
    pub source: &'a Path,
    pub dest: &'a Path,
    /// Snapped back to the previous keyframe before anything is written.
    pub start_ms: u64,
    pub end_ms: u64,
}

/// What actually landed, for the caller's `ClipMeta`.
pub struct ExportOutcome {
    /// Measured from the samples written, not from the request: the in-point
    /// snapped backwards, so the output is usually a little longer than asked.
    pub duration_ms: u64,
    pub video_packets: usize,
    pub has_audio: bool,
}

/// Stream-copies `start_ms..end_ms` of `source` into a new MP4 at `dest`.
///
/// The in-point snaps back to the previous keyframe (spec §6.3). The out-point
/// does not need snapping: cutting after a P-frame is fine, every frame in the
/// output still has its reference.
pub fn export_fast(request: FastExport<'_>) -> Result<ExportOutcome> {
    let keys = keyframes_ms(request.source)?;
    let start_ms = snap_start(&keys, request.start_ms);
    let start_100ns = (start_ms as i64) * 10_000;
    let end_100ns = (request.end_ms as i64) * 10_000;

    let reader = open_compressed(request.source, true)?;

    // The source's own types, declared as both the stream type and the input
    // type. That pairing is what tells the sink writer "do not transform this"
    // -- the same guaranteed-passthrough trick `ClipMuxer::new` uses with the
    // encoder's negotiated H.264 type.
    let video_type = unsafe { reader.GetNativeMediaType(video_stream(), 0) }
        .context("this clip has no video track")?;
    let audio_type: Option<IMFMediaType> =
        unsafe { reader.GetNativeMediaType(audio_stream(), 0) }.ok();

    let writer = unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        MFCreateSinkWriterFromURL(&HSTRING::from(request.dest), None, Some(&attrs))
            .with_context(|| format!("could not create {}", request.dest.display()))?
    };

    let (video_out, audio_out) = unsafe {
        let video_out = writer.AddStream(&video_type).context("AddStream(export video)")?;
        writer
            .SetInputMediaType(video_out, &video_type, None)
            .context("SetInputMediaType(export video passthrough)")?;
        let audio_out = match &audio_type {
            Some(t) => match writer.AddStream(t) {
                Ok(s) => writer.SetInputMediaType(s, t, None).ok().map(|()| s),
                // A source whose AAC type the MP4 sink will not take back
                // verbatim is a silent export, not a failed one. The video is
                // the point; losing the audio track is worth reporting in the
                // outcome, not worth refusing the whole export over.
                Err(_) => None,
            },
            None => None,
        };
        writer.BeginWriting().context("BeginWriting(export)")?;
        (video_out, audio_out)
    };

    let video = copy_stream(
        &reader,
        &writer,
        video_stream(),
        video_out,
        start_100ns,
        end_100ns,
        request.source,
    )?;

    if let Some(audio_out) = audio_out {
        // Failures here are not fatal for the same reason as above: the video
        // track is already written and finalizing will still produce a clip.
        let _ = copy_stream(
            &reader,
            &writer,
            audio_stream(),
            audio_out,
            start_100ns,
            end_100ns,
            request.source,
        );
    }

    unsafe { writer.Finalize().context("Finalize(export)")? };

    if video.packets == 0 {
        // An empty output is worse than no output: it lands in the library as a
        // clip that will not play.
        let _ = std::fs::remove_file(request.dest);
        anyhow::bail!("that range contains no video");
    }

    Ok(ExportOutcome {
        duration_ms: (video.written_100ns / 10_000).max(0) as u64,
        video_packets: video.packets,
        has_audio: audio_out.is_some(),
    })
}

struct CopiedStream {
    packets: usize,
    written_100ns: i64,
}

/// Reads one stream start-to-finish, writing only the samples inside the range
/// and rebasing their timestamps so the output starts at zero.
#[allow(clippy::too_many_arguments)]
fn copy_stream(
    reader: &IMFSourceReader,
    writer: &IMFSinkWriter,
    in_stream: u32,
    out_stream: u32,
    start_100ns: i64,
    end_100ns: i64,
    source: &Path,
) -> Result<CopiedStream> {
    let mut packets = 0usize;
    let mut written_100ns = 0i64;
    let mut empty_reads = 0u32;
    loop {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        unsafe { reader.ReadSample(in_stream, 0, None, Some(&mut flags), None, Some(&mut sample)) }
            .with_context(|| format!("reading {} failed", source.display()))?;
        if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
            break;
        }
        let Some(sample) = sample else {
            empty_reads += 1;
            if empty_reads > MAX_CONSECUTIVE_EMPTY_READS {
                anyhow::bail!("{} stopped delivering samples", source.display());
            }
            continue;
        };
        empty_reads = 0;
        let t = timing(&sample);
        if t.pts_100ns < start_100ns {
            continue;
        }
        if t.pts_100ns >= end_100ns {
            break;
        }
        unsafe {
            sample.SetSampleTime(t.pts_100ns - start_100ns).context("rebasing the timestamp")?;
            writer.WriteSample(out_stream, &sample).context("WriteSample(export)")?;
        }
        packets += 1;
        written_100ns = (t.pts_100ns - start_100ns) + t.duration_100ns;
    }
    Ok(CopiedStream { packets, written_100ns })
}
```

- [ ] **Step 4: Verify it builds and the by-hand test passes**

Run: `cargo build -p trix-core`

Expected: builds clean.

Then, with `TRIX_TEST_CLIP` set to a real clip of at least 4 seconds:

Run: `cargo test -p trix-core -- --ignored fast_export_of_a_real_clip --nocapture`

Expected: PASS.

**If the audio passthrough is the thing that fails** (the MP4 sink refusing the source's AAC type back verbatim is the one genuinely uncertain step in this plan): the export still succeeds silently, and `has_audio` comes back `false`. That is the designed fallback, not a bug to chase. Report it — the follow-up is to decode AAC to PCM and re-encode through `ClipMuxer`'s existing audio path, which is a separate task and not this plan's.

- [ ] **Step 5: Commit**

```bash
git add crates/trix-core/src/export.rs
git commit -m "feat(export): stream-copy a keyframe-snapped range to a new mp4"
```

---

### Task 3: Protocol commands

**Files:**
- Modify: `crates/trix-proto/src/command.rs`
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Produces: `Command::LibraryKeyframes { clip_id }` and `Command::LibraryExport { clip_id, start_ms, end_ms, mode }` where `mode: String`.

**Note:** `mode` is parsed but not validated here. `Command::parse` is a syntax layer — the daemon decides which modes it can actually perform, the same way it, and not this file, decides which config values are in range.

- [ ] **Step 1: Write the failing tests**

Add to `crates/trix-proto/src/command.rs`'s test module (match the existing tests' style — read two of them first):

```rust
    #[test]
    fn library_keyframes_needs_a_clip_id() {
        let req = Request {
            id: 1,
            cmd: "library.keyframes".into(),
            args: serde_json::from_str(r#"{"clip_id":"20260822_101500"}"#).unwrap(),
        };
        assert_eq!(
            Command::parse(&req),
            Ok(Command::LibraryKeyframes { clip_id: "20260822_101500".into() })
        );

        let bare = Request { id: 1, cmd: "library.keyframes".into(), args: Map::new() };
        assert!(Command::parse(&bare).is_err(), "a missing clip_id must be an error response");
    }

    #[test]
    fn library_export_defaults_mode_to_fast() {
        let req = Request {
            id: 2,
            cmd: "library.export".into(),
            args: serde_json::from_str(
                r#"{"clip_id":"20260822_101500","start_ms":1000,"end_ms":4000}"#,
            )
            .unwrap(),
        };
        assert_eq!(
            Command::parse(&req),
            Ok(Command::LibraryExport {
                clip_id: "20260822_101500".into(),
                start_ms: 1000,
                end_ms: 4000,
                mode: "fast".into(),
            }),
            "an omitted mode is fast -- the only mode that exists"
        );
    }

    #[test]
    fn library_export_requires_both_bounds() {
        let req = Request {
            id: 3,
            cmd: "library.export".into(),
            args: serde_json::from_str(r#"{"clip_id":"20260822_101500","start_ms":1000}"#).unwrap(),
        };
        let error = Command::parse(&req).expect_err("a missing end_ms must be refused");
        assert!(error.contains("end_ms"), "the message must name the missing argument: {error}");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p trix-proto`

Expected: FAIL — `no variant named LibraryKeyframes found for enum Command`.

- [ ] **Step 3: Add the variants and the parsing**

In the `Command` enum, replace the doc comment that reads "`library.export` is deliberately absent — it arrives with trim support" (it has now arrived) and add the variants after `LibraryReveal`:

```rust
    /// Every keyframe position in a clip, in milliseconds. The trim bar draws
    /// a tick at each one and snaps the in-point to them (spec §6.3).
    LibraryKeyframes {
        clip_id: String,
    },
    /// Exports `start_ms..end_ms` of a clip as a **new clip in the library**,
    /// answering with its `ClipMeta`.
    ///
    /// No `dest`: the daemon allocates the id and the path, because an export
    /// that landed outside the clip directory would be invisible to every
    /// other `library.*` command. `mode` is on the wire so that `precise`
    /// slots in without a protocol change, but the daemon currently answers
    /// anything other than `fast` with an error.
    LibraryExport {
        clip_id: String,
        start_ms: u64,
        end_ms: u64,
        mode: String,
    },
```

In `Command::parse`, next to the other `library.*` arms:

```rust
            "library.keyframes" => Self::LibraryKeyframes { clip_id: string_arg(req, "library.keyframes", "clip_id")? },
            "library.export" => Self::LibraryExport {
                clip_id: string_arg(req, "library.export", "clip_id")?,
                start_ms: u64_arg(req, "library.export", "start_ms")?,
                end_ms: u64_arg(req, "library.export", "end_ms")?,
                mode: req
                    .args
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("fast")
                    .to_string(),
            },
```

**Before writing `string_arg` / `u64_arg`:** the existing `library.delete` and `library.rename` arms already extract a required `clip_id` and a required string. Read them and reuse whatever helper they use. Only add these two helpers if no equivalent exists, and put them beside the ones that do:

```rust
fn string_arg(req: &Request, cmd: &str, key: &str) -> Result<String, String> {
    match req.args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(format!("{cmd}: \"{key}\" must be a non-empty string")),
    }
}

fn u64_arg(req: &Request, cmd: &str, key: &str) -> Result<u64, String> {
    match req.args.get(key).and_then(Value::as_u64) {
        Some(v) => Ok(v),
        None => Err(format!("{cmd}: \"{key}\" must be a non-negative integer")),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p trix-proto`

Expected: all pass. The dispatcher will not compile yet — `Daemon::dispatch`'s match is deliberately exhaustive with no catch-all arm, so adding a variant breaks it until Task 4. That is the design working: fix it in Task 4, not with a `_ =>` arm here.

- [ ] **Step 5: Commit**

```bash
git add crates/trix-proto/src/command.rs
git commit -m "feat(proto): add library.keyframes and library.export"
```

---

### Task 4: Daemon wiring

**Files:**
- Modify: `crates/trix-daemon/src/state.rs`
- Modify: `crates/trix-daemon/src/dispatch.rs`
- Test: the existing `#[cfg(test)] mod tests` in `crates/trix-daemon/src/dispatch.rs`

**Interfaces:**
- Consumes: `trix_core::export::{export_fast, keyframes_ms, clamp_range, FastExport}`; `trix_core::library::{allocate_clip_id, mp4_path, thumb_path, write_sidecar, read_sidecar, sidecar_path, now_rfc3339_local, is_valid_id}`.
- Produces: `Daemon::keyframes(&self, clip_id: &str) -> Result<Value, String>` and `Daemon::export_clip(&self, clip_id: &str, start_ms: u64, end_ms: u64, mode: &str) -> Result<ClipMeta, String>`.

**Behaviour the tests below pin down:**
- An invalid or unknown `clip_id` is a plain error, refused before any path is joined — the same boundary `library.delete` already enforces.
- `mode: "precise"` is refused by name, so the error tells a third-party client what is actually going on rather than "invalid mode".
- A successful export broadcasts `clip_saved` with the new meta, so every connected UI updates through the handler it already has.

- [ ] **Step 1: Write the failing tests**

Add to `crates/trix-daemon/src/dispatch.rs`'s test module:

```rust
    #[test]
    fn library_export_refuses_precise_mode_by_name() {
        let daemon = idle("export-precise");
        let response = daemon.dispatch(
            1,
            &request_with(
                1,
                "library.export",
                &[
                    ("clip_id", Value::from("20260822_101500")),
                    ("start_ms", Value::from(0)),
                    ("end_ms", Value::from(2000)),
                    ("mode", Value::from("precise")),
                ],
            ),
        );
        assert!(!response.ok, "precise mode is not implemented and must not pretend to be");
        let error = response.error.unwrap_or_default();
        assert!(
            error.contains("precise"),
            "the error must name the mode so a client knows what to change: {error}"
        );
    }

    #[test]
    fn library_export_refuses_a_hostile_clip_id_before_touching_the_filesystem() {
        let daemon = idle("export-hostile");
        let response = daemon.dispatch(
            1,
            &request_with(
                1,
                "library.export",
                &[
                    ("clip_id", Value::from("../../windows/system32/config")),
                    ("start_ms", Value::from(0)),
                    ("end_ms", Value::from(2000)),
                ],
            ),
        );
        assert!(!response.ok, "a traversal id must be refused at the boundary");
    }

    #[test]
    fn library_keyframes_refuses_an_unknown_clip() {
        let daemon = idle("keyframes-unknown");
        let response = daemon.dispatch(
            1,
            &request_with(1, "library.keyframes", &[("clip_id", Value::from("20260822_101500"))]),
        );
        assert!(!response.ok, "a clip that is not there has no keyframes");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p trix-daemon`

Expected: FAIL to compile — the `dispatch` match is not exhaustive over the two new `Command` variants.

- [ ] **Step 3: Implement the daemon methods**

Add to `crates/trix-daemon/src/state.rs`, beside `rename` / `set_favorite` / `reveal` (read `reveal` first — it already does the id validation and path resolution this needs, and the new methods must match it rather than invent a second style):

```rust
    /// Every keyframe position in a clip, for the trim bar's ticks.
    pub(crate) fn keyframes(&self, clip_id: &str) -> Result<Value, String> {
        // `paths_for` is the sealed choke point: it runs `library::is_valid_id`
        // before it builds a single path, so a hostile id is refused here with
        // no I/O. Never reach a clip file any other way -- see the `mod
        // clip_paths` doc comment for why that seal exists.
        let paths = self.paths_for(clip_id).map_err(|e| format!("{e:#}"))?;
        if !paths.mp4().is_file() {
            return Err(format!("no clip {clip_id}"));
        }
        let keys = trix_core::export::keyframes_ms(paths.mp4()).map_err(|e| format!("{e:#}"))?;
        let mut out = Map::new();
        out.insert("clip_id".to_string(), Value::from(clip_id));
        out.insert("keyframes".to_string(), Value::from(keys));
        Ok(Value::Object(out))
    }

    /// Exports a range of a clip as a new clip in the library.
    ///
    /// Synchronous, like `clip`: a fast-mode export is a stream copy of a few
    /// seconds and finishes well inside a socket round trip, so there is no
    /// worker thread and no progress event to subscribe to.
    pub(crate) fn export_clip(
        &self,
        clip_id: &str,
        start_ms: u64,
        end_ms: u64,
        mode: &str,
    ) -> Result<ClipMeta, String> {
        if mode != "fast" {
            return Err(format!(
                "export mode \"{mode}\" is not supported yet; only \"fast\" is implemented"
            ));
        }
        let paths = self.paths_for(clip_id).map_err(|e| format!("{e:#}"))?;
        if !paths.mp4().is_file() {
            return Err(format!("no clip {clip_id}"));
        }
        // The source's own sidecar carries the width/height/fps/encoder the
        // export inherits verbatim -- a stream copy changes none of them.
        let source_meta = trix_core::library::read_sidecar(paths.sidecar())
            .map_err(|e| format!("{clip_id} has no readable metadata: {e:#}"))?;
        let (start_ms, end_ms) =
            trix_core::export::clamp_range(source_meta.duration_ms, start_ms, end_ms)?;

        let dir = paths.dir();
        let new_id = trix_core::library::allocate_clip_id(dir).map_err(|e| format!("{e:#}"))?;
        let dest = trix_core::library::mp4_path(dir, &new_id);
        let outcome = trix_core::export::export_fast(trix_core::export::FastExport {
            source: paths.mp4(),
            dest: &dest,
            start_ms,
            end_ms,
        })
        .map_err(|e| format!("{e:#}"))?;

        // The source's thumbnail, copied. It is the source's first frame rather
        // than the trim's, which is wrong in the strict sense -- but a grid card
        // with no image at all reads as a broken clip, and this export has no
        // decoder to pull the real frame from. Best-effort: a clip without a
        // preview is still a clip.
        let _ = std::fs::copy(paths.thumb(), trix_core::library::thumb_path(dir, &new_id));

        let meta = ClipMeta {
            id: new_id.clone(),
            title: format!("{} (trimmed)", source_meta.title),
            created: trix_core::library::now_rfc3339_local(),
            duration_ms: outcome.duration_ms,
            bytes: std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0),
            width: source_meta.width,
            height: source_meta.height,
            fps: source_meta.fps,
            encoder: source_meta.encoder.clone(),
            has_audio: outcome.has_audio,
            favorite: false,
        };
        if let Err(e) = trix_core::library::write_sidecar(dir, &meta) {
            tracing::warn!(clip = %new_id, error = %format!("{e:#}"), "export saved without a sidecar");
        }

        // The same event a hotkey clip fires, so every open UI prepends the new
        // clip through the handler it already has. No export-specific event and
        // no UI change needed for the grid to update.
        self.clients.broadcast(&Event::new(
            "clip_saved",
            serde_json::to_value(&meta).unwrap_or(Value::Null),
        ));
        Ok(meta)
    }
```

**Do not build paths any other way.** `Daemon::paths_for(id) -> anyhow::Result<ClipPaths>` (`state.rs:1376`) is the only route to a clip's paths: `ClipPaths` is sealed in a child module so its fields cannot be constructed by hand, and `ClipPaths::for_id` runs `library::is_valid_id` before building anything. That is what makes the hostile-id test in Step 1 pass without either new method validating anything itself. `mp4()`, `sidecar()`, `thumb()`, and `dir()` all return `&Path`, so pass them straight through rather than taking another reference.

Then in `crates/trix-daemon/src/dispatch.rs`, beside the other `Library*` arms:

```rust
            Ok(Command::LibraryKeyframes { clip_id }) => match self.keyframes(&clip_id) {
                Ok(data) => Response::ok(request.id, data),
                Err(e) => Response::err(request.id, e),
            },
            Ok(Command::LibraryExport { clip_id, start_ms, end_ms, mode }) => {
                updated_clip(request.id, self.export_clip(&clip_id, start_ms, end_ms, &mode))
            }
```

`updated_clip` already serializes a bare `ClipMeta` into an ok response — the same shape `library.rename` answers with. Confirm that before reusing it.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p trix-daemon`

Expected: all pass, including the three new ones.

- [ ] **Step 5: Verify the whole workspace still builds**

Run: `cargo build --workspace`

Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-daemon/src/state.rs crates/trix-daemon/src/dispatch.rs
git commit -m "feat(daemon): answer library.keyframes and library.export"
```

---

### Task 5: The trim bar

**Files:**
- Create: `crates/trix-ui/web/src/components/TrimBar.svelte`
- Modify: `crates/trix-ui/web/src/lib/state.svelte.ts`
- Modify: `crates/trix-ui/web/src/views/ClipPage.svelte`
- Test: `crates/trix-ui/web/src/lib/state.svelte.test.ts` (follow the existing tests' mocking of `call`)

**Interfaces:**
- Consumes: `call` from `../lib/ipc`; `app.current`, `app.toast` from `../lib/state.svelte`.
- Produces: `app.keyframesFor(id: string): Promise<number[]>`, `app.exportTrim(id: string, startMs: number, endMs: number): Promise<void>`.

- [ ] **Step 1: Write the component**

Create `crates/trix-ui/web/src/components/TrimBar.svelte`:

```svelte
<script lang="ts">
  // Presentational on purpose: this component never calls the daemon. It is
  // given a duration, a playhead, a keyframe list and the current in/out, and
  // it reports back where the user dragged. That keeps every daemon call in
  // state.svelte.ts, where the rest of them already live.
  let {
    durationMs,
    playheadMs,
    keyframes,
    inMs,
    outMs,
    onchange,
    onseek,
  }: {
    durationMs: number;
    playheadMs: number;
    keyframes: number[];
    inMs: number;
    outMs: number;
    onchange: (inMs: number, outMs: number) => void;
    onseek: (ms: number) => void;
  } = $props();

  const pct = (ms: number) => (durationMs > 0 ? (ms / durationMs) * 100 : 0);

  /** Latest keyframe at or before `ms` -- the same rule the daemon applies, so
      the handle sits where the export will actually cut rather than where the
      pointer was released. */
  function snap(ms: number): number {
    let best = 0;
    for (const k of keyframes) if (k <= ms) best = k;
    return best;
  }

  function msAt(e: MouseEvent, el: HTMLElement): number {
    const box = el.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (e.clientX - box.left) / box.width));
    return Math.round(ratio * durationMs);
  }
</script>

<div class="trim">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="track" onclick={(e) => onseek(msAt(e, e.currentTarget))}>
    {#each keyframes as k (k)}
      <span class="tick" style="left: {pct(k)}%"></span>
    {/each}
    <span class="range" style="left: {pct(inMs)}%; width: {pct(outMs - inMs)}%"></span>
    <span class="playhead" style="left: {pct(playheadMs)}%"></span>
  </div>

  <div class="handles">
    <label>
      In
      <input
        type="range" min="0" max={durationMs} step="1" value={inMs}
        oninput={(e) => onchange(snap(Number(e.currentTarget.value)), outMs)} />
    </label>
    <label>
      Out
      <input
        type="range" min="0" max={durationMs} step="1" value={outMs}
        oninput={(e) => onchange(inMs, Number(e.currentTarget.value))} />
    </label>
  </div>
</div>

<style>
  .trim { margin: 12px 0; display: grid; gap: 6px; }
  .track { position: relative; height: 26px; border-radius: 6px; background: var(--panel); border: 1px solid var(--line); cursor: pointer; overflow: hidden; }
  .tick { position: absolute; top: 0; bottom: 0; width: 1px; background: var(--line); }
  .range { position: absolute; top: 0; bottom: 0; background: color-mix(in srgb, var(--accent) 28%, transparent); border-left: 2px solid var(--accent); border-right: 2px solid var(--accent); }
  .playhead { position: absolute; top: 0; bottom: 0; width: 2px; background: var(--text); }
  .handles { display: flex; gap: 16px; }
  .handles label { flex: 1; display: flex; align-items: center; gap: 8px; color: var(--dim); font-size: 12px; }
  .handles input { flex: 1; }
</style>
```

- [ ] **Step 2: Write the failing state test**

Add to `crates/trix-ui/web/src/lib/state.svelte.test.ts`, matching how the existing tests stub `call`:

```ts
  it('exportTrim refuses a range whose out point is not after its in point', async () => {
    const calls: string[] = [];
    // ... stub `call` the way the neighbouring tests do, pushing cmd into calls
    await app.exportTrim('20260822_101500', 4000, 4000);
    expect(calls).not.toContain('library.export');
  });
```

- [ ] **Step 3: Run it to verify it fails**

Run: `npx vitest run` from `crates/trix-ui/web`

Expected: FAIL — `app.exportTrim is not a function`.

- [ ] **Step 4: Add the two state methods**

In `crates/trix-ui/web/src/lib/state.svelte.ts`, beside `rename` / `setFavorite` / `reveal`:

```ts
  async keyframesFor(id: string): Promise<number[]> {
    try {
      const data = await call<{ keyframes: number[] }>('library.keyframes', { clip_id: id });
      return data.keyframes ?? [];
    } catch (e) {
      // A clip whose keyframes cannot be read is still playable, so this
      // degrades to a bar with no ticks rather than an error the user must
      // dismiss before they can watch anything.
      console.warn('keyframes unavailable', e);
      return [];
    }
  }

  async exportTrim(id: string, startMs: number, endMs: number) {
    if (endMs <= startMs) {
      this.toast('error', 'Set the out point after the in point.');
      return;
    }
    try {
      // The daemon broadcasts clip_saved for the new clip, and wireDaemon
      // already prepends on that event -- so nothing here reloads the grid.
      await call('library.export', {
        clip_id: id,
        start_ms: Math.round(startMs),
        end_ms: Math.round(endMs),
        mode: 'fast',
      });
      this.toast('ok', 'Trimmed clip saved.');
    } catch (e) {
      this.toast('error', String(e));
    }
  }
```

Check `toast`'s real signature before using `'ok'` — match whatever the existing success toasts pass.

- [ ] **Step 5: Wire it into the clip page**

In `crates/trix-ui/web/src/views/ClipPage.svelte`, replace the reserved-slot comment with the bar, and add the state and keys. Import at the top:

```ts
  import TrimBar from '../components/TrimBar.svelte';
```

Add state beside the existing `let` declarations:

```ts
  let keyframes = $state<number[]>([]);
  let inMs = $state(0);
  let outMs = $state(0);
  let playheadMs = $state(0);
  let exporting = $state(false);

  // Re-fetch whenever the page shows a different clip. Prev/next is a UI-side
  // repoint of the same component, so without this the bar would keep the
  // previous clip's ticks and trim points.
  $effect(() => {
    const id = clip?.id;
    if (!id) return;
    inMs = 0;
    outMs = clip?.duration_ms ?? 0;
    playheadMs = 0;
    void (async () => {
      keyframes = await app.keyframesFor(id);
    })();
  });

  async function exportTrim() {
    if (!clip || exporting) return;
    exporting = true;
    try {
      await app.exportTrim(clip.id, inMs, outMs);
    } finally {
      exporting = false;
    }
  }
```

Replace the reserved-slot comment block with:

```svelte
  {#if clip.duration_ms > 0}
    <TrimBar
      durationMs={clip.duration_ms}
      {playheadMs}
      {keyframes}
      {inMs}
      {outMs}
      onchange={(i, o) => { inMs = i; outMs = o; }}
      onseek={(ms) => { if (video) video.currentTime = ms / 1000; }} />
  {/if}
```

Add `ontimeupdate` to the existing `<video>` element so the playhead tracks:

```svelte
    <video bind:this={video} src={clipUrl(app.clipDir, clip.id)} controls autoplay
      ontimeupdate={() => { if (video) playheadMs = video.currentTime * 1000; }}></video>
```

Add an export button to the action row, beside Rename:

```svelte
      <button onclick={exportTrim} disabled={confirmingDelete || exporting}>
        {exporting ? 'Exporting…' : 'Export trimmed'}
      </button>
```

Add the keys to the existing `switch` in `onkeydown`, and handle `Ctrl+E` before the switch (it carries a modifier, which the switch's plain `e.key` cases do not express):

```ts
    if (e.key === 'e' && e.ctrlKey) {
      e.preventDefault();
      void exportTrim();
      return;
    }
```

```ts
      case 'i':
        inMs = playheadMs;
        break;
      case 'o':
        outMs = playheadMs;
        break;
```

**Check `shouldHandleKey` first** (`crates/trix-ui/web/src/lib/keys.ts`): `i` and `o` are ordinary printable characters, so they must not fire while the rename input has focus. If the existing guard already covers that (it exists precisely to stop window-level shortcuts stealing from focused controls), change nothing.

- [ ] **Step 6: Verify**

Run, from `crates/trix-ui/web`:

```bash
npm run check
npx vitest run
npm run build
```

Expected: 0 errors, all tests pass, build succeeds.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-ui/web/src
git commit -m "feat(ui): trim bar with keyframe ticks and export"
```

---

### Task 6: Documentation and the hand-verification gate

**Files:**
- Modify: `docs/ship/README.txt`
- Modify: `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md`

- [ ] **Step 1: Correct the shipped README**

`docs/ship/README.txt:200` currently reads "No trimming or exporting yet. Clips are saved whole, at the length you…". Read the surrounding lines and replace it with what is now true — fast trimming exists, in-points snap to the nearest second, and precise (frame-accurate) trimming does not exist yet.

- [ ] **Step 2: Mark the spec's deferred lines as delivered**

The spec says in several places that trim/export "arrive with the plan that follows" stage 4. Add a short status note under §6.3 recording that fast mode shipped on 2026-08-22, that precise mode did not, and listing the four deviations from the table at the top of this plan. Do not rewrite the spec's design text — the record of what was designed stays intact.

- [ ] **Step 3: Hand-verification (a person must do this — no script covers it)**

Build and run the real app:

```bash
cargo build --release --workspace
cd crates/trix-ui && cargo tauri build --no-bundle
```

Then, with a real clip of at least 5 seconds in the library, check each of:

- [ ] Opening a clip shows the bar with ticks roughly one second apart.
- [ ] Dragging **In** snaps to a tick; dragging **Out** moves freely.
- [ ] `I` and `O` set the points at the playhead; `Space` still plays and pauses.
- [ ] `Ctrl+E` and the button both export, and the new clip appears in the grid **without a manual refresh**.
- [ ] The exported clip plays, starts at the in-point, and is about the expected length.
- [ ] **The exported clip has sound.** This is the one uncertain step in the plan — if it is silent, the AAC passthrough fell back, which Task 2 documents.
- [ ] The source clip is untouched: same size, still plays in full.
- [ ] Export while **armed** — it must not disturb the replay ring or drop the arm state.

- [ ] **Step 4: Commit**

```bash
git add docs
git commit -m "docs: record that fast-mode trimming shipped"
```

---

## Self-Review

**Spec coverage.** §6.2's trim bar → Task 5. §6.3's keyframe ticks and snapping → Tasks 1, 4, 5. §6.3's fast stream copy → Task 2. §6.3's "re-encode runs in the daemon" → Task 4 (the export runs there; the re-encode itself is out of scope by decision). `I`/`O`/`Ctrl+E` from §6.2's key table → Task 5. Deliberately not covered, and recorded in the deviations table: `dest`, `export_progress`/`export_done`, `trim_mode`, precise mode, decoded filmstrip thumbnails.

**Known soft spots, stated rather than hidden:**
- **AAC passthrough is the one step I could not verify from the codebase.** Task 2 has a documented fallback and Task 6 has an explicit hand-check for it.
- **Two "read the neighbouring code first" instructions** — `string_arg`/`u64_arg` and `toast`'s signature — are deliberate. Those helpers exist in forms this plan could not confirm exactly, and inventing a second copy would be worse than looking. (`paths_for` was confirmed and is named exactly.)
- **`keyframes_ms` runs a full demux pass per clip opened.** Cheap at replay-buffer length, and uncached on purpose. If clips ever get long, this is the first thing to cache.
