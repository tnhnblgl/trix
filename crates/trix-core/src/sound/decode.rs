//! Turning whatever the user picked into the one format `PlaySound` can play.
//!
//! Media Foundation is the decoder, which is why this costs no dependency: the
//! project already links it for encoding, and it brings mp3, m4a, wma and flac
//! with it. `IMFSourceReader` is asked for 16-bit PCM directly, so the
//! resampling and channel conversion happen inside MF rather than here.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaBuffer, IMFSample, IMFSourceReader, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION,
    MFAudioFormat_PCM, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
    MFSTARTUP_NOSOCKET, MFShutdown, MFStartup,
};
use windows::core::HSTRING;

use super::{BITS_PER_SAMPLE, CHANNELS, MAX_PCM_BYTES, SAMPLE_RATE, capped, wav_from_pcm};

/// How many consecutive `ReadSample` calls may come back with neither a
/// sample nor the end-of-stream flag before `to_wav` gives up on the file.
///
/// A stream tick or a format-change gap is a normal, occasional event; a real
/// file might produce a few in a row around a bitrate switch, but not
/// thousands. This is set generous enough that no real file should ever come
/// close, while still bounding a truncated download or a container Media
/// Foundation only half-recognises, either of which can otherwise return
/// this result forever. That matters here specifically because the only
/// caller runs `to_wav` while the daemon's config mutex is held, so an
/// unbounded loop would not just fail this request -- it would wedge every
/// `status` and `config.get` call, and the clip path itself, until the
/// process was killed.
const MAX_CONSECUTIVE_EMPTY_READS: u32 = 1_000;

/// Advances the consecutive-empty-read counter, bailing with `path` named
/// once [`MAX_CONSECUTIVE_EMPTY_READS`] is exceeded.
///
/// Pulled out of the read loop so this bound can be unit-tested without
/// Media Foundation: the loop below calls this exact function rather than
/// repeating the check inline.
fn count_empty_read(consecutive_empty_reads: u32, path: &Path) -> Result<u32> {
    let consecutive_empty_reads = consecutive_empty_reads + 1;
    if consecutive_empty_reads > MAX_CONSECUTIVE_EMPTY_READS {
        bail!(
            "{} never finished decoding: Media Foundation returned no audio \
             after {MAX_CONSECUTIVE_EMPTY_READS} consecutive reads",
            path.display()
        );
    }
    Ok(consecutive_empty_reads)
}

/// Media Foundation started for the duration of a call, and stopped on the way
/// out however the call ends.
///
/// A guard rather than a pair of calls because every `?` between them would
/// otherwise leak an MF startup count. `probe.rs` does the same thing by hand
/// in one function; this one has a dozen fallible steps.
///
/// Must be declared before any COM interface it outlives. `to_wav` binds
/// `_session` before `reader`, `wanted`, and the loop's locals, and Rust
/// drops bindings in reverse declaration order, so every interface is
/// released before `MFShutdown` runs. Move this binding later and shutdown
/// would run while an interface is still alive.
struct Session;

impl Session {
    fn start() -> Result<Self> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.context("MFStartup failed")?;
        Ok(Self)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
    }
}

/// Decodes `path` and returns it as WAV bytes, capped at [`super::MAX_SECONDS`].
///
/// The error is shown to the user in the settings page, so it names the file
/// and carries Media Foundation's own reason: "that file is not a sound Windows
/// can read" is actionable, "0xC00D36C4" is not, and this returns both.
pub fn to_wav(path: &Path) -> Result<Vec<u8>> {
    // Before MF is even started: a path that is simply not there is the common
    // mistake, and it deserves a plain answer rather than a codec's.
    if !path.is_file() {
        bail!("{} is not a file", path.display());
    }

    let _session = Session::start()?;

    let reader: IMFSourceReader =
        unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(path), None) }
            .with_context(|| format!("Windows could not read {} as a sound", path.display()))?;

    // Asking for PCM here is what makes MF do the work: it inserts whatever
    // decoder and resampler the source needs to reach this format, so an mp3
    // at 48 kHz mono is asked to arrive as 44.1 kHz stereo 16-bit without
    // anything below needing to know it was ever an mp3. That is the intent,
    // not an observed fact: the only test that runs against a real Media
    // Foundation reader (`a_real_file_decodes_to_a_capped_wav`) uses a WAV
    // fixture already in this exact format, so the resampling and channel
    // conversion described above are not exercised by any test.
    let wanted = unsafe { MFCreateMediaType() }.context("MFCreateMediaType failed")?;
    unsafe {
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).context("setting major type")?;
        wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).context("setting subtype")?;
        wanted
            .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, u32::from(BITS_PER_SAMPLE))
            .context("setting bit depth")?;
        wanted
            .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)
            .context("setting sample rate")?;
        wanted
            .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(CHANNELS))
            .context("setting channel count")?;
    }

    let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
    unsafe { reader.SetCurrentMediaType(stream, None, &wanted) }
        .with_context(|| format!("{} has no audio Windows can decode", path.display()))?;

    let mut pcm: Vec<u8> = Vec::new();
    let mut consecutive_empty_reads: u32 = 0;
    loop {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        unsafe { reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample)) }
            .with_context(|| format!("reading decoded audio from {} failed", path.display()))?;

        if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
            break;
        }
        // A read can legitimately return no sample -- a format change or a gap
        // -- without being the end of the stream. Skipping is correct; treating
        // it as the end would truncate the sound at the first hiccup. But it is
        // bounded: see `MAX_CONSECUTIVE_EMPTY_READS`.
        let Some(sample) = sample else {
            consecutive_empty_reads = count_empty_read(consecutive_empty_reads, path)?;
            continue;
        };
        consecutive_empty_reads = 0;

        let buffer: IMFMediaBuffer =
            unsafe { sample.ConvertToContiguousBuffer() }.context("ConvertToContiguousBuffer")?;
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut length = 0u32;
        unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }
            .context("IMFMediaBuffer::Lock")?;
        // The copy happens before `Unlock`, and `Unlock` happens before the
        // next iteration can lock anything else. `data` is only valid between
        // the two.
        pcm.extend_from_slice(unsafe { std::slice::from_raw_parts(data, length as usize) });
        unsafe { buffer.Unlock() }.context("IMFMediaBuffer::Unlock")?;

        // Stop reading rather than decode a whole album and throw it away.
        if pcm.len() >= MAX_PCM_BYTES {
            break;
        }
    }

    if pcm.is_empty() {
        bail!("{} contains no audio", path.display());
    }

    Ok(wav_from_pcm(&capped(pcm), CHANNELS, SAMPLE_RATE, BITS_PER_SAMPLE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs without Media Foundation: a path that does not exist is refused
    /// before anything is started, so this is the one decode test CI can hold.
    #[test]
    fn a_missing_file_is_refused_by_name() {
        let err = to_wav(std::path::Path::new(r"Z:\no\such\sound.mp3")).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("sound.mp3"), "the error must name the file: {message}");
    }

    /// The bound from finding 1: a long run of legitimate gaps must not trip
    /// it (a real file can have a few), but it must still end the loop
    /// eventually so a truncated download or a half-recognised container
    /// cannot hang `to_wav` forever while the daemon holds the config lock.
    /// This calls the exact function the read loop calls, so it exercises the
    /// real guard without needing Media Foundation.
    #[test]
    fn the_empty_read_guard_bails_after_the_limit_but_not_before() {
        let path = std::path::Path::new(r"C:\sounds\fixture.mp3");

        let mut consecutive_empty_reads = 0u32;
        for _ in 0..MAX_CONSECUTIVE_EMPTY_READS {
            consecutive_empty_reads = count_empty_read(consecutive_empty_reads, path)
                .expect("must not bail before the limit is exceeded");
        }

        let err = count_empty_read(consecutive_empty_reads, path).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("fixture.mp3"), "the error must name the file: {message}");
        assert!(
            message.contains(&MAX_CONSECUTIVE_EMPTY_READS.to_string()),
            "the error should say how many reads it gave up after: {message}"
        );
    }

    /// A single empty read, or a handful, must not bail -- resetting on a
    /// successful sample is what lets a long file with occasional legitimate
    /// gaps still decode fully. This only checks the counter does not fire
    /// early; the reset itself is a plain assignment in the loop.
    #[test]
    fn a_handful_of_empty_reads_does_not_bail() {
        let path = std::path::Path::new(r"C:\sounds\fixture.mp3");

        let mut consecutive_empty_reads = 0u32;
        for _ in 0..5 {
            consecutive_empty_reads = count_empty_read(consecutive_empty_reads, path)
                .expect("a few gaps in a row must not bail");
        }
        assert_eq!(consecutive_empty_reads, 5);
    }

    /// Needs Media Foundation and a real decoder, so it is `#[ignore]`d and run
    /// by hand -- the same treatment the encoder gets. Generate the fixture
    /// with the chime example, then convert it by hand to mp3 if you have a
    /// converter; a wav alone still exercises the source reader end to end.
    #[test]
    #[ignore = "needs Media Foundation; run by hand with --ignored"]
    fn a_real_file_decodes_to_a_capped_wav() {
        let fixture = std::env::temp_dir().join("trix-decode-fixture.wav");
        let pcm = vec![0u8; 4 * 44_100 * 30]; // 30 s of silence, well over the cap
        std::fs::write(&fixture, super::super::wav_from_pcm(&pcm, 2, 44_100, 16)).unwrap();

        let wav = to_wav(&fixture).expect("a wav file must decode");

        assert!(super::super::looks_like_wav(&wav));
        assert_eq!(
            wav.len(),
            44 + super::super::MAX_PCM_BYTES,
            "a 30-second source must come back capped at 10 seconds"
        );
        let _ = std::fs::remove_file(&fixture);
    }
}
