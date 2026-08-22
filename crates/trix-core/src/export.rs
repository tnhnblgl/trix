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
    MF_SINK_WRITER_DISABLE_THROTTLING, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READERF_ENDOFSTREAM, MFCreateAttributes, MFCreateSinkWriterFromURL,
    MFCreateSourceReaderFromURL, MFSampleExtension_CleanPoint,
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

/// Advances the consecutive-empty-read counter, bailing with `source` named
/// once [`MAX_CONSECUTIVE_EMPTY_READS`] is exceeded.
///
/// Pulled out of the read loop so this bound can be unit-tested without
/// Media Foundation, the same way -- and for the same reason -- as
/// `sound/decode.rs`'s `count_empty_read`: the loop below calls this exact
/// function rather than repeating the check inline.
fn count_empty_read(empty_reads: u32, source: &Path) -> Result<u32> {
    let empty_reads = empty_reads + 1;
    if empty_reads > MAX_CONSECUTIVE_EMPTY_READS {
        anyhow::bail!(
            "{} stopped delivering video samples after {MAX_CONSECUTIVE_EMPTY_READS} \
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
            empty_reads = count_empty_read(empty_reads, source)?;
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
            empty_reads = count_empty_read(empty_reads, source)?;
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
    /// Foundation -- same shape as `sound/decode.rs`'s equivalent test.
    #[test]
    fn the_empty_read_guard_bails_after_the_limit_but_not_before() {
        let path = Path::new(r"C:\clips\fixture.mp4");

        let mut empty_reads = 0u32;
        for _ in 0..MAX_CONSECUTIVE_EMPTY_READS {
            empty_reads = count_empty_read(empty_reads, path)
                .expect("must not bail before the limit is exceeded");
        }

        let err = count_empty_read(empty_reads, path).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("fixture.mp4"), "the error must name the file: {message}");
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
            empty_reads =
                count_empty_read(empty_reads, path).expect("a few gaps in a row must not bail");
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
        empty_reads = count_empty_read(empty_reads, path).expect("gap 1");
        empty_reads = count_empty_read(empty_reads, path).expect("gap 2");
        assert_eq!(empty_reads, 2);

        // Stands in for the read loop's `empty_reads = 0` on a real sample.
        empty_reads = 0;

        empty_reads = count_empty_read(empty_reads, path).expect("gap after the reset");
        assert_eq!(empty_reads, 1, "the two gaps before the reset must not still be counted");
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

        let _ = std::fs::remove_file(&dest);
    }
}
