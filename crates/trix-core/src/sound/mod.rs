//! The sound Trix plays when it saves a clip: the WAV container it always
//! plays from, and the decoder that gets a user's own file into that shape.
//!
//! Playback is `PlaySound`, which decodes WAV and nothing else, so every
//! custom sound is converted once when it is chosen rather than decoded on
//! every clip. That is what keeps the clip path a single call with no codec,
//! no buffer lifetime and no device to manage.

pub mod decode;

/// The format every converted sound is written in. CD rate, stereo, 16-bit:
/// chosen as an ordinary target for `MFAudioFormat_PCM` to resample into and
/// for a Windows audio device to accept without a format negotiation. That is
/// the intent, not an observed fact -- see `decode::to_wav`'s doc comment for
/// the same caveat on the read side: the resampling path this format asks for
/// has never actually run under a test.
pub const SAMPLE_RATE: u32 = 44_100;
pub const CHANNELS: u16 = 2;
pub const BITS_PER_SAMPLE: u16 = 16;

/// How much of a chosen file is used.
///
/// A notification sound is short. Without a cap, picking a five-minute mp3
/// would silently write ~50 MB into `%APPDATA%`; truncating is friendlier than
/// refusing, because someone who picks a song wants its opening.
pub const MAX_SECONDS: u32 = 10;

/// The cap in bytes of PCM. A whole number of frames by construction, so a
/// truncation can never leave half a sample behind — half a sample is a click.
pub const MAX_PCM_BYTES: usize = (SAMPLE_RATE as usize)
    * (CHANNELS as usize)
    * (BITS_PER_SAMPLE as usize / 8)
    * (MAX_SECONDS as usize);

/// The 44-byte canonical WAV header, followed by `pcm` unchanged.
///
/// Written by hand rather than with a crate because it is fourteen fields of
/// little-endian integers and the alternative is a dependency in a project
/// whose whole pitch is being small.
pub fn wav_from_pcm(pcm: &[u8], channels: u16, sample_rate: u32, bits_per_sample: u16) -> Vec<u8> {
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);

    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    // Everything after this field: the 4-byte "WAVE" tag, the 24-byte fmt
    // chunk, the 8-byte data header, and the payload.
    out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunks are 16 bytes
    out.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Whether these bytes open like a WAV file.
///
/// Deliberately only the container tags: this is asked of files Trix itself
/// wrote, to catch a truncated or corrupted cache, not to validate a stranger's
/// audio. The decoder is the judge of what a user's file is.
pub fn looks_like_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

/// Cuts PCM down to [`MAX_PCM_BYTES`].
pub fn capped(mut pcm: Vec<u8>) -> Vec<u8> {
    pcm.truncate(MAX_PCM_BYTES);
    pcm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header is 44 bytes and every field in it is derived, so a wrong
    /// `byte_rate` or `block_align` produces a file Windows plays at the wrong
    /// speed rather than one it refuses -- which is why each field is asserted
    /// individually rather than against a golden blob.
    #[test]
    fn wav_header_describes_the_payload() {
        let pcm = vec![0u8; 8];
        let wav = wav_from_pcm(&pcm, 2, 44_100, 16);

        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(wav[16..20].try_into().unwrap()), 16);
        assert_eq!(
            u16::from_le_bytes(wav[20..22].try_into().unwrap()),
            1,
            "format tag must be PCM"
        );
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 2, "channels");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 44_100, "sample rate");
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 176_400, "byte rate");
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 4, "block align");
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16, "bits per sample");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 8, "data length");
        assert_eq!(&wav[44..], &pcm[..], "the payload must follow the header unchanged");
    }

    #[test]
    fn a_mono_header_uses_the_mono_block_align() {
        let wav = wav_from_pcm(&[0u8; 4], 1, 22_050, 16);
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 2, "block align");
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 44_100, "byte rate");
    }

    #[test]
    fn our_own_output_is_recognised_as_a_wav() {
        assert!(looks_like_wav(&wav_from_pcm(&[0u8; 4], 1, 22_050, 16)));
    }

    /// The sniff exists to reject files that are not WAVs at all. Each case
    /// below is one a user could really hand us: a renamed text file, a
    /// truncated download, and nothing at all.
    #[test]
    fn non_wav_bytes_are_rejected() {
        assert!(!looks_like_wav(b"this is not audio, it is a text file"));
        assert!(!looks_like_wav(b"RIFF"), "a 4-byte truncation has no WAVE tag to read");
        assert!(!looks_like_wav(b""));
        assert!(!looks_like_wav(b"RIFF____AVI "), "a RIFF container that is not WAVE");
    }

    #[test]
    fn the_cap_truncates_only_what_is_over_it() {
        let over = capped(vec![7u8; MAX_PCM_BYTES + 5_000]);
        assert_eq!(over.len(), MAX_PCM_BYTES, "a long sound is cut to exactly the cap");

        let under = capped(vec![7u8; 100]);
        assert_eq!(under.len(), 100, "a short sound is untouched");
    }

    /// A cap that landed mid-frame would leave half a sample at the end, which
    /// plays as a click. 10 s at 44.1 kHz stereo 16-bit is a whole number of
    /// 4-byte frames, and this is what keeps that true if a constant changes.
    #[test]
    fn the_cap_is_a_whole_number_of_frames() {
        let frame = usize::from(CHANNELS) * usize::from(BITS_PER_SAMPLE) / 8;
        assert_eq!(MAX_PCM_BYTES % frame, 0);
        assert_eq!(MAX_PCM_BYTES, 44_100 * 2 * 2 * 10);
    }
}
