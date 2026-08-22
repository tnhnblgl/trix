//! Fast-mode clip export: a keyframe-snapped stream copy of an existing MP4.
//!
//! Spec §6.3's `fast` mode. No decoder and no encoder is involved: an
//! `IMFSourceReader` hands over the source's already-encoded H.264 and AAC
//! samples and an `IMFSinkWriter` writes them straight back out. That is what
//! makes an export sub-second and lossless, and it is also why the in-point can
//! only land on a keyframe -- nothing in this pipeline could synthesize the
//! frames between one keyframe and the next.

use std::path::Path;

use anyhow::{Context as _, Result};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader,
    MF_READWRITE_DISABLE_CONVERTERS, MF_SINK_WRITER_DISABLE_THROTTLING,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MFCreateAttributes,
    MFCreateSinkWriterFromURL, MFCreateSourceReaderFromURL, MFSampleExtension_CleanPoint,
};
use windows::core::HSTRING;

use crate::encode::mf::ensure_mf_started;

/// Shortest export worth writing. Below this the output is a file with one or
/// two frames in it, which is a mistake rather than a clip.
pub const MIN_TRIM_MS: u64 = 200;

/// Latest keyframe at or before `start_ms`.
///
/// Snapping *backwards* rather than to the nearest keyframe is deliberate: a
/// forward snap silently drops footage the user explicitly asked to keep, and
/// the frame they set the in-point on is usually the one that matters.
pub fn snap_start(keyframes_ms: &[u64], start_ms: u64) -> u64 {
    keyframes_ms.iter().copied().rfind(|&k| k <= start_ms).unwrap_or(0)
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

/// A read that returns no sample without being the end of the stream is legal
/// (a format change, a gap). Skipping is correct; treating it as the end would
/// truncate. Bounded so a source that only ever returns nothing cannot spin
/// forever -- the same reasoning as `sound/decode.rs`'s
/// `MAX_CONSECUTIVE_EMPTY_READS`, but a smaller number. `to_wav`'s loop runs
/// while the daemon's config mutex is held, so a hang there wedges every
/// `status` and `config.get` call, and the clip path too -- that severity is
/// why it is set generous, at 1,000. This loop holds no lock and blocks
/// nothing but its own caller, so a tighter bound is deliberate here.
const MAX_CONSECUTIVE_EMPTY_READS: u32 = 64;

/// Advances the consecutive-empty-read counter, bailing with `source` and
/// `label` named once [`MAX_CONSECUTIVE_EMPTY_READS`] is exceeded. `label`
/// names which stream was being read ("video" or "audio"), since both
/// `keyframes_ms`'s video-only pass and `copy_stream`'s pass over either
/// track call this same function.
///
/// Pulled out of the read loop so this bound can be unit-tested without
/// Media Foundation, the same way -- and for the same reason -- as
/// `sound/decode.rs`'s `count_empty_read`: every read loop in this file
/// calls this exact function rather than repeating the check inline.
fn count_empty_read(empty_reads: u32, source: &Path, label: &str) -> Result<u32> {
    let empty_reads = empty_reads + 1;
    if empty_reads > MAX_CONSECUTIVE_EMPTY_READS {
        anyhow::bail!(
            "{} stopped delivering {label} samples after {MAX_CONSECUTIVE_EMPTY_READS} \
             consecutive empty reads",
            source.display()
        );
    }
    Ok(empty_reads)
}

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
    // `keyframes_ms` only needs the position, not the length. Task 2's remux
    // reads this same field to reproduce each sample's duration in the copy,
    // so it stays on the struct rather than being trimmed to what this file
    // alone uses.
    #[allow(dead_code)]
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
            empty_reads = count_empty_read(empty_reads, source, "video")?;
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

/// What to export, and to where.
pub struct FastExport<'a> {
    pub source: &'a Path,
    pub dest: &'a Path,
    /// Snapped back to the previous keyframe before anything is written.
    pub start_ms: u64,
    pub end_ms: u64,
}

/// What actually landed, for the caller's `ClipMeta`.
#[derive(Debug)]
pub struct ExportOutcome {
    /// Measured from the samples written, not from the request: the in-point
    /// snapped backwards, so the output is usually a little longer than asked.
    pub duration_ms: u64,
    pub video_packets: usize,
    pub has_audio: bool,
}

/// True when `dest` and `source` are two spellings of one file.
///
/// A plain `==` only catches the identical spelling, and
/// `MFCreateSinkWriterFromURL` truncates on call: `clips\a.mp4` against
/// `clips\sub\..\a.mp4`, or against the same path in different case, would
/// destroy the very clip being exported. `canonicalize` resolves all of those,
/// but it opens the file to do it, so it fails on a `dest` that does not exist
/// yet -- which is the ordinary case, and a path that cannot be opened at all
/// is not the source clip. Hence the plain comparison first and the canonical
/// one only as a second opinion.
fn names_the_same_file(dest: &Path, source: &Path) -> bool {
    if dest == source {
        return true;
    }
    match (dest.canonicalize(), source.canonicalize()) {
        (Ok(dest), Ok(source)) => dest == source,
        _ => false,
    }
}

/// How far an attempt got before it failed, which is the only thing the retry
/// needs in order to know whether `dest` is its to delete.
enum Failed {
    /// Failed before `MFCreateSinkWriterFromURL` was called, so `dest` was
    /// never created and never truncated. Anything sitting at that path
    /// belongs to whoever put it there, and deleting it on the way out of a
    /// failure would be destroying a stranger's file.
    BeforeCreating(anyhow::Error),
    /// Failed at or after the call that creates `dest`. Whatever is there now
    /// is this attempt's own half-written output -- truncated, missing its
    /// `moov` atom -- and Task 4 hands exports straight to `library::scan`,
    /// which adopts any bare .mp4 it finds as a real clip. It has to go.
    AfterCreating(anyhow::Error),
}

/// One attempt's result. Named because `Result` in this module is
/// `anyhow::Result`, and an attempt's error deliberately is not an
/// `anyhow::Error`: it carries where the failure landed as well as what it
/// was.
type Attempt = std::result::Result<ExportOutcome, Failed>;

/// Stream-copies `start_ms..end_ms` of `source` into a new MP4 at `dest`.
///
/// The in-point snaps back to the previous keyframe (spec 6.3). The out-point
/// does not need snapping: cutting after a P-frame is fine, every frame in the
/// output still has its reference.
///
/// Audio is attempted but never fatal: if an export carrying an audio stream
/// fails at any stage, whatever it left at `dest` is deleted and the whole
/// export runs again with no audio stream declared, reporting
/// `has_audio: false`. See [`with_video_only_retry`].
pub fn export_fast(request: FastExport<'_>) -> Result<ExportOutcome> {
    // `MFCreateSinkWriterFromURL` truncates whatever `dest` names the moment
    // it is called. Nothing downstream of this function would notice or refuse
    // an export written over the clip it is reading -- this is the one place
    // that must.
    if names_the_same_file(request.dest, request.source) {
        anyhow::bail!("the export destination cannot be the source clip");
    }

    // Deliberately before any attempt: a source that cannot even be indexed
    // must fail with `dest` untouched, rather than after a sink writer has
    // already truncated it.
    let keys = keyframes_ms(request.source)?;
    let start_ms = snap_start(&keys, request.start_ms);
    let start_100ns = (start_ms as i64) * 10_000;
    let end_100ns = (request.end_ms as i64) * 10_000;

    with_video_only_retry(request.dest, request.source, |with_audio| {
        attempt_export(request.source, request.dest, start_100ns, end_100ns, with_audio)
    })
}

/// Runs `attempt` with audio and, if that fails after `dest` was created,
/// deletes what it left behind and runs it once more with no audio stream at
/// all.
///
/// This is the entire audio-failure policy, in one place, and it is one
/// mechanism rather than a guard per call site because the MP4 sink defers its
/// real validation: measured, it accepts at `AddStream` and
/// `SetInputMediaType` types it can still refuse later, and it will even get
/// past `BeginWriting` with a stream that has an output type and no input
/// type. So the door is not where a refusal shows up, and which later call it
/// shows up at is not something this module can know. Re-running the whole
/// export covers every landing site there is, including the ones nobody has
/// thought of yet; guarding one call site, as three earlier rounds of fixes
/// did, leaves the rest turning "no audio" into "no export".
///
/// The price: a source with no audio stream at all makes the second attempt
/// identical to the first, so a genuine failure -- an empty range, an
/// unreadable source -- is paid for twice. That is a path that fails either
/// way, and one extra demux pass is cheaper than classifying errors by whether
/// audio could have caused them, which is exactly the guesswork this replaces.
///
/// `attempt` is a parameter rather than an inlined call so that this policy --
/// which error the caller ends up seeing, and precisely which failures delete
/// `dest` -- can be tested without Media Foundation.
fn with_video_only_retry(
    dest: &Path,
    source: &Path,
    mut attempt: impl FnMut(bool) -> Attempt,
) -> Result<ExportOutcome> {
    let first = match attempt(true) {
        Ok(outcome) => return Ok(outcome),
        // Nothing was created, so there is nothing to delete -- and nothing a
        // video-only retry would do differently either: every step before the
        // sink writer exists is on the source side, and the audio-specific
        // ones there already swallow their own failures (`open_compressed`'s
        // stream selection, `prepare`'s `GetNativeMediaType` for audio).
        Err(Failed::BeforeCreating(err)) => return Err(err),
        Err(Failed::AfterCreating(err)) => err,
    };

    // Before the retry rather than after it. `MFCreateSinkWriterFromURL`
    // truncates `dest` when the second attempt opens it, so leaving the first
    // attempt's carcass in place would usually be harmless -- but not if the
    // retry then failed before reaching that call, which would leave the first
    // attempt's broken file on disk for `library::scan` to adopt.
    discard(dest);
    tracing::warn!(
        clip = %source.display(),
        error = %format!("{first:#}"),
        "export with audio failed, retrying video-only"
    );

    match attempt(false) {
        Ok(outcome) => Ok(outcome),
        // Deleted above, and this attempt never got as far as recreating it.
        Err(Failed::BeforeCreating(err)) => Err(err),
        Err(Failed::AfterCreating(err)) => {
            discard(dest);
            Err(err)
        }
    }
}

/// Deletes a half-written `dest`, and says so in the log if it cannot.
///
/// The delete is best-effort by necessity — the export has already failed and
/// nothing better is going to happen by failing harder — but *silently*
/// best-effort was the wrong shape. Windows Defender routinely still holds a
/// handle on a file it has just finished scanning, so `remove_file` comes back
/// with a sharing violation, and a discarded error left the truncated MP4 on
/// disk with nothing anywhere explaining where it came from. (`library::scan`
/// now skips zero-byte `.mp4`s, which covers the commonest shape of leftover;
/// this covers the rest, and turns either into something diagnosable.)
///
/// `NotFound` is not a failure: it is the state this function exists to reach.
fn discard(dest: &Path) {
    match std::fs::remove_file(dest) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            path = %dest.display(),
            error = %e,
            "could not delete a failed export; a broken .mp4 may be left behind"
        ),
    }
}

/// One whole export attempt, from opening the source through `Finalize`.
///
/// An attempt opens its own reader rather than sharing one: this module never
/// seeks (the source reader's `SetCurrentPosition` sits behind a cargo feature
/// this workspace does not enable), so a reader already read to the out-point
/// has nothing left to give a retry.
fn attempt_export(
    source: &Path,
    dest: &Path,
    start_100ns: i64,
    end_100ns: i64,
    with_audio: bool,
) -> Attempt {
    let (reader, video_type, audio_type) =
        prepare(source, with_audio).map_err(Failed::BeforeCreating)?;
    run_export(&reader, &video_type, audio_type.as_ref(), dest, start_100ns, end_100ns, source)
        .map_err(Failed::AfterCreating)
}

/// Opens `source` for compressed reading and collects the media types the
/// export will declare.
///
/// Every step is on the source side. Nothing here touches `dest`, which is
/// what lets [`attempt_export`] label a failure from this function as "nothing
/// landed".
fn prepare(
    source: &Path,
    with_audio: bool,
) -> Result<(IMFSourceReader, IMFMediaType, Option<IMFMediaType>)> {
    let reader = open_compressed(source, with_audio)?;

    // The source's own types, handed on to `open_sink` to be declared as both
    // the stream type and the input type. That pairing is what tells the sink
    // writer "do not transform this" -- the same guaranteed-passthrough trick
    // `ClipMuxer::new` uses with the encoder's negotiated H.264 type.
    let video_type = unsafe { reader.GetNativeMediaType(video_stream(), 0) }
        .context("this clip has no video track")?;
    // `GetNativeMediaType` describes the streams the file has and does not
    // care whether `open_compressed` managed to select them, so this can hand
    // back a type for an audio stream that will not actually read. Detecting
    // that here is not worth it: the retry covers it, along with every other
    // way audio can go wrong later on.
    let audio_type = match with_audio {
        true => unsafe { reader.GetNativeMediaType(audio_stream(), 0) }.ok(),
        false => None,
    };

    Ok((reader, video_type, audio_type))
}

/// Everything from creating the sink writer through `Finalize`. Split out of
/// [`attempt_export`] so that every failure from the moment `dest` is created
/// -- the create call itself included, since it truncates `dest` and can still
/// fail after that -- is one `Err` with one meaning. This owns the writer by
/// value, so any early return, `?` on a mid-copy failure included, drops it,
/// and the file handle it holds, before the caller deletes the file.
fn run_export(
    reader: &IMFSourceReader,
    video_type: &IMFMediaType,
    audio_type: Option<&IMFMediaType>,
    dest: &Path,
    start_100ns: i64,
    end_100ns: i64,
    source: &Path,
) -> Result<ExportOutcome> {
    let (writer, video_out, audio_out) = open_sink(dest, video_type, audio_type)?;

    let video = copy_stream(
        reader,
        &writer,
        video_stream(),
        video_out,
        start_100ns,
        end_100ns,
        source,
        "video",
    )?;

    if video.packets == 0 {
        // An empty output is worse than no output: it lands in the library as
        // a clip that will not play. Checked here rather than after
        // `Finalize`, because a sink that has been handed no samples at all
        // fails `Finalize` with MF_E_SINK_NO_SAMPLES_PROCESSED (0xC00D4A44)
        // -- measured against this codepath, not assumed -- and the caller
        // deserves this sentence rather than that HRESULT. Video is copied
        // first, so zero packets here means the sink has seen nothing.
        anyhow::bail!("that range contains no video");
    }

    // An audio failure propagates, unlike the empty audio range below: `?`
    // here takes the whole attempt down so `export_fast` can delete it and
    // re-run video-only. Carrying on instead would finalize a file whose audio
    // stops partway through, which no `has_audio` value describes honestly.
    let has_audio = match audio_out {
        Some(stream) => {
            let audio = copy_stream(
                reader,
                &writer,
                audio_stream(),
                stream,
                start_100ns,
                end_100ns,
                source,
                "audio",
            )?;
            if audio.packets == 0 {
                // A declared stream that receives no sample is fine by the
                // container: measured, an export that writes video samples and
                // no audio samples finalizes cleanly, and the result re-indexes
                // as a playable, video-only MP4. So an empty audio range is
                // reported through `has_audio` rather than retried.
                tracing::warn!(
                    clip = %source.display(),
                    "audio stream was declared but the range produced no audio samples"
                );
            }
            audio.packets > 0
        }
        None => false,
    };

    unsafe { writer.Finalize().context("Finalize(export)")? };

    Ok(ExportOutcome {
        duration_ms: (video.written_100ns / 10_000).max(0) as u64,
        video_packets: video.packets,
        has_audio,
    })
}

/// Creates the sink writer at `dest`, declares the video stream and -- when
/// `audio_type` is given -- the audio stream, and returns the writer past
/// `BeginWriting`.
///
/// Each stream is declared with the *same* `IMFMediaType` object as both the
/// `AddStream` type and the `SetInputMediaType` type. That pairing is the
/// passthrough guarantee: the sink writer has no transform to insert when the
/// input type it is handed is already the output type it was asked for.
///
/// It also gets `MF_READWRITE_DISABLE_CONVERTERS`, which turns that reasoning
/// from an assumption into something the platform enforces. Handed an input
/// type it cannot pass straight through, a sink writer's *documented*
/// behaviour is to quietly insert a converter — for H.264 that means a full
/// encoder MFT, and fast mode would become a re-encode: seconds instead of
/// milliseconds, a generation of quality gone, and not one line anywhere
/// saying so. The pairing above should make that unreachable, but "should"
/// covers a lot of ground once the MP4 sink normalizes a type or a future
/// Windows build rewrites an attribute. With this set, the day the short
/// circuit stops holding is the day `SetInputMediaType` returns an error and
/// the export fails loudly, which is what this module's doc comments have been
/// claiming all along.
///
/// The writer gets `MF_SINK_WRITER_DISABLE_THROTTLING`, exactly as
/// `ClipMuxer::new` does and for the same reason: this module writes one whole
/// track and then the other, and a throttled writer blocks `WriteSample`
/// waiting for the lagging stream to catch up.
///
/// Every step propagates its failure, audio included. There is nothing to be
/// gained by guarding the audio calls here: the MP4 sink accepts types at
/// `AddStream` and `SetInputMediaType` that it can still refuse later, so a
/// refusal is as likely to land in `copy_stream` or at `Finalize` as in this
/// function. [`with_video_only_retry`] is what turns any of them into a silent
/// export rather than a failed one.
fn open_sink(
    dest: &Path,
    video_type: &IMFMediaType,
    audio_type: Option<&IMFMediaType>,
) -> Result<(IMFSinkWriter, u32, Option<u32>)> {
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        // The passthrough guarantee, enforced rather than assumed. See above.
        attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)?;
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(dest), None, Some(&attrs))
            .with_context(|| format!("could not create {}", dest.display()))?;

        let video_out = writer.AddStream(video_type).context("AddStream(export video)")?;
        writer
            .SetInputMediaType(video_out, video_type, None)
            .context("SetInputMediaType(export video passthrough)")?;

        let audio_out = match audio_type {
            Some(audio_type) => {
                let stream = writer.AddStream(audio_type).context("AddStream(export audio)")?;
                writer
                    .SetInputMediaType(stream, audio_type, None)
                    .context("SetInputMediaType(export audio passthrough)")?;
                Some(stream)
            }
            None => None,
        };

        writer.BeginWriting().context("BeginWriting(export)")?;
        Ok((writer, video_out, audio_out))
    }
}

struct CopiedStream {
    packets: usize,
    written_100ns: i64,
}

/// Reads one stream start-to-finish, writing only the samples inside the range
/// and rebasing their timestamps so the output starts at zero. `label`
/// ("video" or "audio") is only for `count_empty_read`'s error message.
#[allow(clippy::too_many_arguments)]
fn copy_stream(
    reader: &IMFSourceReader,
    writer: &IMFSinkWriter,
    in_stream: u32,
    out_stream: u32,
    start_100ns: i64,
    end_100ns: i64,
    source: &Path,
    label: &str,
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
            empty_reads = count_empty_read(empty_reads, source, label)?;
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

    /// The bound from `MAX_CONSECUTIVE_EMPTY_READS`: a long run of legitimate
    /// gaps must not trip it (a real file can have a few), but it must still
    /// end the loop eventually so a source that only ever returns nothing
    /// cannot hang `keyframes_ms` forever. This calls the exact function the
    /// read loop calls, so it exercises the real guard without needing Media
    /// Foundation -- same shape as `sound/decode.rs`'s equivalent test. Uses
    /// the "audio" label (rather than "video", as every other test here
    /// does) so this test also pins that the label actually reaches the
    /// message, not just that a message is produced.
    #[test]
    fn the_empty_read_guard_bails_after_the_limit_but_not_before() {
        let path = Path::new(r"C:\clips\fixture.mp4");

        let mut empty_reads = 0u32;
        for _ in 0..MAX_CONSECUTIVE_EMPTY_READS {
            empty_reads = count_empty_read(empty_reads, path, "audio")
                .expect("must not bail before the limit is exceeded");
        }

        let err = count_empty_read(empty_reads, path, "audio").unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("fixture.mp4"), "the error must name the file: {message}");
        assert!(message.contains("audio"), "the error must name which stream stalled: {message}");
        assert!(
            message.contains(&MAX_CONSECUTIVE_EMPTY_READS.to_string()),
            "the error should say how many reads it gave up after: {message}"
        );
    }

    /// A single empty read, or a handful, must not bail -- resetting on a
    /// successful sample is what lets a long clip with occasional legitimate
    /// gaps still index fully. This only checks the counter does not fire
    /// early; the reset itself is a plain assignment in the loop, covered
    /// below.
    #[test]
    fn a_handful_of_empty_reads_does_not_bail() {
        let path = Path::new(r"C:\clips\fixture.mp4");

        let mut empty_reads = 0u32;
        for _ in 0..5 {
            empty_reads = count_empty_read(empty_reads, path, "video")
                .expect("a few gaps in a row must not bail");
        }
        assert_eq!(empty_reads, 5);
    }

    /// `sound/decode.rs`'s equivalent test left its reset-on-success line
    /// uncovered, because a bare `empty_reads = 0` triggered by a real sample
    /// is hard to reach without Media Foundation. This pins the interaction
    /// the reset exists for at the level that *can* be checked without MF:
    /// two gaps, then a reset standing in for the loop's `empty_reads = 0` on
    /// a real sample, then one more gap. If the reset did not actually zero
    /// the counter, this would report 4 rather than the true 1 -- and on a
    /// real file, a bound sized for the gaps after one reset would then fire
    /// too early on a clip with several periodic, legitimate gaps.
    #[test]
    fn a_reset_between_gaps_means_only_the_later_gaps_count() {
        let path = Path::new(r"C:\clips\fixture.mp4");

        let mut empty_reads = 0u32;
        empty_reads = count_empty_read(empty_reads, path, "video").expect("gap 1");
        empty_reads = count_empty_read(empty_reads, path, "video").expect("gap 2");
        assert_eq!(empty_reads, 2);

        // Stands in for the read loop's `empty_reads = 0` on a real sample.
        empty_reads = 0;

        empty_reads = count_empty_read(empty_reads, path, "video").expect("gap after the reset");
        assert_eq!(empty_reads, 1, "the two gaps before the reset must not still be counted");
    }

    /// Recognisable content for a file the export did not write. Every
    /// destination-side assertion below turns on being able to tell "this file
    /// is still the one the test put there" from "this file is something the
    /// export produced" -- and, when the path is gone, on knowing that the
    /// only thing that could have removed it is a delete.
    const STUB: &[u8] = b"not a clip -- seeded by the test";

    /// A private directory under the system temp dir, per test. Nothing here
    /// ever reads or writes the developer's real config or clip library.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("trix-export-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating the scratch dir");
        dir
    }

    fn an_outcome(has_audio: bool) -> ExportOutcome {
        ExportOutcome { duration_ms: 2_000, video_packets: 120, has_audio }
    }

    /// The retry, in the shape the real export uses it: the attempt that has
    /// audio creates `dest` and then fails, and the video-only attempt has to
    /// find the path clear before it recreates it -- because
    /// `MFCreateSinkWriterFromURL` truncates rather than refuses, so a
    /// leftover would be silently written over instead of cleaned up.
    #[test]
    fn a_failed_first_attempt_is_deleted_and_retried_video_only() {
        let dir = scratch_dir("retry");
        let dest = dir.join("out.mp4");
        let mut attempts: Vec<bool> = Vec::new();

        let outcome = with_video_only_retry(&dest, Path::new("clip.mp4"), |with_audio| {
            attempts.push(with_audio);
            if with_audio {
                std::fs::write(&dest, b"half-written first attempt").unwrap();
                Err(Failed::AfterCreating(anyhow::anyhow!("WriteSample(export)")))
            } else {
                assert!(
                    !dest.exists(),
                    "the first attempt's output must be gone before the retry recreates it"
                );
                std::fs::write(&dest, b"video-only export").unwrap();
                Ok(an_outcome(false))
            }
        })
        .expect("the video-only retry must succeed");

        assert_eq!(attempts, vec![true, false], "exactly one retry, and it drops the audio");
        assert!(!outcome.has_audio, "a video-only export must report has_audio: false");
        assert_eq!(std::fs::read(&dest).unwrap(), b"video-only export");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When even the video-only attempt fails, the caller gets *that* error --
    /// the audio attempt's is only a warning in the log -- and nothing is left
    /// on disk for `library::scan` to adopt.
    #[test]
    fn both_attempts_failing_reports_the_second_and_leaves_nothing_behind() {
        let dir = scratch_dir("both-fail");
        let dest = dir.join("out.mp4");

        let err = with_video_only_retry(&dest, Path::new("clip.mp4"), |with_audio| {
            std::fs::write(&dest, b"half-written").unwrap();
            let which = if with_audio { "the audio attempt" } else { "the video-only attempt" };
            Err(Failed::AfterCreating(anyhow::anyhow!("{which} failed")))
        })
        .unwrap_err();

        assert!(
            format!("{err:#}").contains("the video-only attempt"),
            "the caller must see the last error, not the first: {err:#}"
        );
        assert!(!dest.exists(), "a wholly failed export must leave no file at all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failure before the sink writer exists is not an audio failure and
    /// `dest` is not the export's to delete: whatever is at that path was put
    /// there by somebody else, and this export never touched it.
    #[test]
    fn a_failure_before_the_sink_exists_neither_retries_nor_deletes() {
        let dir = scratch_dir("before-creating");
        let dest = dir.join("out.mp4");
        std::fs::write(&dest, STUB).unwrap();
        let mut attempts: Vec<bool> = Vec::new();

        let err = with_video_only_retry(&dest, Path::new("clip.mp4"), |with_audio| {
            attempts.push(with_audio);
            Err(Failed::BeforeCreating(anyhow::anyhow!("Windows could not open clip.mp4")))
        })
        .unwrap_err();

        assert_eq!(attempts, vec![true], "a video-only retry would fail identically");
        assert!(format!("{err:#}").contains("could not open"));
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            STUB,
            "a file this export never created must survive its failure untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ordinary path: audio works, so there is no second attempt and the
    /// output stays exactly as the first one wrote it.
    #[test]
    fn an_export_that_works_the_first_time_is_never_retried() {
        let dir = scratch_dir("first-time");
        let dest = dir.join("out.mp4");
        let mut attempts: Vec<bool> = Vec::new();

        let outcome = with_video_only_retry(&dest, Path::new("clip.mp4"), |with_audio| {
            attempts.push(with_audio);
            std::fs::write(&dest, b"an export with audio").unwrap();
            Ok(an_outcome(true))
        })
        .expect("a working export must not be disturbed");

        assert_eq!(attempts, vec![true], "no retry when the first attempt succeeds");
        assert!(outcome.has_audio);
        assert_eq!(std::fs::read(&dest).unwrap(), b"an export with audio");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `dest == source` compared as written misses a second spelling of the
    /// same file, and the cost of missing it is the source clip truncated by
    /// `MFCreateSinkWriterFromURL` -- the export destroying the thing it was
    /// asked to copy. Needs no Media Foundation: the refusal happens before
    /// anything is opened.
    #[test]
    fn an_export_onto_the_source_is_refused_however_the_path_is_spelled() {
        let dir = scratch_dir("same-file");
        let clip = dir.join("clip.mp4");
        std::fs::write(&clip, STUB).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();

        // The same file by a different route. `PathBuf` comparison says these
        // are two paths; the filesystem says they are one file.
        let spelled_differently = dir.join("sub").join("..").join("clip.mp4");
        assert_ne!(clip, spelled_differently, "the test needs two spellings, not two paths");

        let err = export_fast(FastExport {
            source: &clip,
            dest: &spelled_differently,
            start_ms: 0,
            end_ms: 1_000,
        })
        .unwrap_err();

        assert!(
            format!("{err:#}").contains("cannot be the source"),
            "the refusal must name the reason: {err:#}"
        );
        assert_eq!(
            std::fs::read(&clip).unwrap(),
            STUB,
            "the source clip must come out of a refused export byte for byte"
        );
        // The other spelling Windows lets through: same name, different case.
        assert!(
            names_the_same_file(&dir.join("CLIP.MP4"), &clip),
            "a case-different path names the same file on this filesystem"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the same guard: an ordinary export writes to a path
    /// that does not exist yet, where `canonicalize` fails on the destination.
    /// If that made the check say "same file", every export would be refused.
    #[test]
    fn a_destination_that_does_not_exist_yet_is_not_the_source() {
        let dir = scratch_dir("distinct");
        let clip = dir.join("clip.mp4");
        std::fs::write(&clip, STUB).unwrap();
        let dest = dir.join("export.mp4");

        assert!(!names_the_same_file(&dest, &clip), "the ordinary export must not be refused");
        assert!(names_the_same_file(&clip, &clip), "the identical spelling is still caught");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pre-creation window, end to end through the public function: the
    /// source does not exist, so `keyframes_ms` fails before any sink writer
    /// is created, and the file already sitting at `dest` must be left exactly
    /// as it was. (No Media Foundation involved -- `open_compressed` refuses a
    /// path that is not a file before it starts MF.)
    #[test]
    fn a_source_that_cannot_be_read_leaves_the_destination_untouched() {
        let dir = scratch_dir("no-source");
        let dest = dir.join("out.mp4");
        std::fs::write(&dest, STUB).unwrap();
        let missing = dir.join("nope.mp4");

        let err =
            export_fast(FastExport { source: &missing, dest: &dest, start_ms: 0, end_ms: 1_000 })
                .unwrap_err();

        assert!(format!("{err:#}").contains("nope.mp4"), "the error must name the source: {err:#}");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            STUB,
            "a failure before `dest` was ever created must not delete what is there"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    /// The other half of the by-hand gate. Exports the middle of a real clip
    /// and asserts the output is a real, shorter, independently indexable MP4
    /// -- and, when the source carries audio, that the audio came with it.
    ///
    /// That last assertion is the only guard `has_audio` has. Its real
    /// computation (`audio.packets > 0`, in `run_export`) needs Media
    /// Foundation and a real AAC track to exercise, so nothing in the ordinary
    /// suite touches it: reverting it to `audio_out.is_some()` -- the exact
    /// regression an earlier fix wave corrected, which reports sound on a clip
    /// that has none -- leaves `cargo test --workspace` entirely green. It also
    /// covers the one thing this feature could not verify up front, which is
    /// whether AAC survives passthrough at all on a given machine.
    ///
    ///   cargo test -p trix-core -- --ignored fast_export_of_a_real_clip
    #[test]
    #[ignore = "needs a real clip; set TRIX_TEST_CLIP"]
    fn fast_export_of_a_real_clip() {
        let Ok(path) = std::env::var("TRIX_TEST_CLIP") else {
            panic!("set TRIX_TEST_CLIP to an .mp4 path");
        };
        let source = Path::new(&path);
        let dir = scratch_dir("real-clip");
        let dest = dir.join("trix-export-test.mp4");

        let outcome =
            export_fast(FastExport { source, dest: &dest, start_ms: 1_000, end_ms: 3_000 })
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

        // Guarded on the source, because TRIX_TEST_CLIP may legitimately point
        // at a clip recorded with the microphone and system audio both off.
        // Asked through `prepare`, which is the same call the export itself
        // uses to decide whether to declare an audio stream, so the two can
        // never disagree about what "the source has audio" means.
        let (_reader, _video, audio_type) =
            prepare(source, true).expect("the source must at least open");
        if audio_type.is_some() {
            assert!(
                outcome.has_audio,
                "the source has an audio track and the export reported none -- either AAC \
                 passthrough was refused on this machine and `with_video_only_retry` silently \
                 dropped the sound, or `has_audio` stopped counting written packets"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A range that snaps to a keyframe at or after every sample in it (here,
    /// `end_ms: 0` against a clip whose first frame is at `pts_100ns == 0`)
    /// writes zero video packets, and a sink handed no samples at all fails
    /// `Finalize` with MF_E_SINK_NO_SAMPLES_PROCESSED. So this exercises the
    /// real failure path end to end -- no synthetic media types, just a range
    /// guaranteed to be empty -- and checks both halves of the answer: the
    /// caller-facing message, and that nothing broken is left on disk for
    /// `library::scan` to adopt.
    ///
    /// `dest` is seeded first, with content nothing else would write. Its
    /// absence at the end can then only be explained by a delete: an assertion
    /// against a path the test itself had cleared could not tell "the export
    /// created a file and cleaned it up" from "the export never got that far".
    ///
    ///   cargo test -p trix-core -- --ignored a_video_less_export_deletes_its_own_output_and_names_the_reason
    #[test]
    #[ignore = "needs a real clip; set TRIX_TEST_CLIP"]
    fn a_video_less_export_deletes_its_own_output_and_names_the_reason() {
        let Ok(path) = std::env::var("TRIX_TEST_CLIP") else {
            panic!("set TRIX_TEST_CLIP to an .mp4 path");
        };
        let source = Path::new(&path);
        let dir = scratch_dir("empty-range");
        let dest = dir.join("trix-export-empty-test.mp4");
        std::fs::write(&dest, STUB).unwrap();

        let err = export_fast(FastExport { source, dest: &dest, start_ms: 0, end_ms: 0 })
            .expect_err("a zero-length range must not produce a clip");
        assert!(
            format!("{err:#}").contains("no video"),
            "the caller must see why, not a raw HRESULT: {err:#}"
        );
        assert!(
            !dest.exists(),
            "the seeded file was truncated by MFCreateSinkWriterFromURL and then had to be \
             deleted; finding the path gone is the proof the delete ran"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
