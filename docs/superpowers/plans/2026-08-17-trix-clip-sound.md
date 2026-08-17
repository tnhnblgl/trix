# Trix Clip Sound Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The daemon plays a short sound when it saves a clip; the user can replace it with their own mp3 or wav and put the original back with one button.

**Architecture:** `trix-daemon` plays the sound, because `trix-ui.exe` is not resident and would be silent mid-game. Custom sounds are decoded once by Media Foundation into a `.wav` copy Trix owns, so playback stays a single `PlaySoundW` call. The UI owns only the settings, reached over the control socket like every other capability.

**Tech Stack:** Rust, `windows` 0.62.2 (`Win32_Media_Audio` for `PlaySoundW`, `Win32_Media_MediaFoundation` for decoding), Svelte 5 + Tauri v2 for the settings page.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-17-trix-clip-sound-design.md`. Read it before Task 1.
- **No new crate dependencies.** Only the `Win32_Media_Audio` feature is added to the existing workspace `windows` entry.
- `trix-ui` must never depend on `trix-core` (desktop spec §3.2). Every UI capability arrives over the control socket.
- `trix-ui` has **no logger** and builds under `windows_subsystem = "windows"`. Any logging macro there is a defect, and any comment claiming something is "logged" is wrong.
- Any test that touches config must use a scratch `%APPDATA%` **and** a scratch `clip_dir`. `%APPDATA%` alone is not isolation: an empty `clip_dir` resolves to the real `%USERPROFILE%\Videos\Trix`. Use the existing `Daemon::new_at(config, None)` and `with_scratch_config(name)` seams in `dispatch.rs`.
- Format the exact files you touched with `rustfmt --edition 2024 <paths>`. **Do not run `cargo fmt -- <paths>`** — cargo passes every crate root regardless of the paths given and reformats unrelated files.
- Clippy gate is **no increase from the 14-line baseline**, not zero. `-D warnings` would fail on pre-existing lints this branch did not introduce.
- Commits: author stays `tnhnblgl <tnhnblgl@gmail.com>`. **No `Co-Authored-By` trailers and no "Generated with Claude Code" lines.** Never pass `--author`.
- Never push, and never `git push --tags`.
- Sound constants, used verbatim everywhere: sample rate **44100**, channels **2**, bits per sample **16**, cap **10 seconds**. The built-in chime is the one exception and is mono at 22050 Hz.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/trix-core/src/sound/mod.rs` | **Create.** Pure WAV container writing/sniffing and the format constants. No Windows APIs, fully unit-testable. |
| `crates/trix-core/src/sound/decode.rs` | **Create.** Media Foundation decode of any supported file to capped PCM, returned as WAV bytes. |
| `crates/trix-core/examples/make-chime.rs` | **Create.** Generates the built-in chime. Run once; the output is committed. |
| `crates/trix-daemon/assets/clip.wav` | **Create.** The built-in chime, embedded with `include_bytes!`. |
| `crates/trix-daemon/src/sound.rs` | **Create.** `PlaySoundW` wrapper and the embedded asset. |
| `crates/trix-core/src/config.rs` | **Modify.** Two keys plus the cache-path helper. |
| `crates/trix-daemon/src/state.rs` | **Modify.** Play on clip save; convert in the `config.set` gate. |
| `crates/trix-daemon/src/folder.rs` | **Modify.** Add the audio-file dialog beside the folder dialog. |
| `crates/trix-daemon/src/window.rs` | **Modify.** New action, and the detached thread that runs the dialog. |
| `crates/trix-daemon/src/dispatch.rs` | **Modify.** `sound.pick` / `sound.test` arms; extract the config broadcast. |
| `crates/trix-proto/src/command.rs` | **Modify.** Two command variants. |
| `crates/trix-ui/web/src/lib/settings.ts` | **Modify.** The `sound` field kind and its two rows. |
| `crates/trix-ui/web/src/components/Field.svelte` | **Modify.** Render the sound row. |
| `crates/trix-ui/web/src/views/Settings.svelte` | **Modify.** Wire Choose/Test/Reset; consume `config_changed`. |
| `docs/ship/README.txt` | **Modify.** User-facing description. |

---

### Task 1: WAV container and the built-in chime

**Files:**
- Create: `crates/trix-core/src/sound/mod.rs`
- Create: `crates/trix-core/examples/make-chime.rs`
- Create: `crates/trix-daemon/assets/clip.wav` (generated, then committed)
- Modify: `crates/trix-core/src/lib.rs`

**Interfaces:**
- Produces: `trix_core::sound::{SAMPLE_RATE, CHANNELS, BITS_PER_SAMPLE, MAX_SECONDS, MAX_PCM_BYTES}`, `wav_from_pcm(pcm: &[u8], channels: u16, sample_rate: u32, bits_per_sample: u16) -> Vec<u8>`, `looks_like_wav(bytes: &[u8]) -> bool`, `capped(pcm: Vec<u8>) -> Vec<u8>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/trix-core/src/sound/mod.rs` with only the tests plus a `mod tests` block referring to functions that do not exist yet:

```rust
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
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1, "format tag must be PCM");
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
```

- [ ] **Step 2: Run the tests to verify they fail**

First register the module. In `crates/trix-core/src/lib.rs`, add `pub mod sound;` after `pub mod replay;` so the list stays alphabetical:

```rust
pub mod replay;
pub mod sound;
pub mod stats;
```

Run: `cargo test -p trix-core sound::`
Expected: FAIL — `cannot find function 'wav_from_pcm' in this scope` and similar for each name.

- [ ] **Step 3: Write the implementation**

Put this above the `#[cfg(test)] mod tests` block in `crates/trix-core/src/sound/mod.rs`:

```rust
//! The sound Trix plays when it saves a clip: the WAV container it always
//! plays from, and the decoder that gets a user's own file into that shape.
//!
//! Playback is `PlaySound`, which decodes WAV and nothing else, so every
//! custom sound is converted once when it is chosen rather than decoded on
//! every clip. That is what keeps the clip path a single call with no codec,
//! no buffer lifetime and no device to manage.

pub mod decode;

/// The format every converted sound is written in. CD rate, stereo, 16-bit:
/// what `MFAudioFormat_PCM` is happiest resampling into and what every
/// Windows audio device accepts without a format negotiation.
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
```

Create `crates/trix-core/src/sound/decode.rs` as an empty placeholder for now, so `pub mod decode;` compiles — Task 4 fills it:

```rust
//! Filled in by Task 4.
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p trix-core sound::`
Expected: PASS, 6 tests.

- [ ] **Step 5: Write the chime generator**

Create `crates/trix-core/examples/make-chime.rs`:

```rust
//! Generates the chime Trix plays when it saves a clip.
//!
//! Run once; its output is committed. It exists so the sound in the repo has a
//! recipe rather than a provenance — nobody has to wonder where a binary blob
//! came from or what licence it carries.
//!
//! Usage: cargo run -p trix-core --example make-chime -- crates/trix-daemon/assets/clip.wav

use std::f32::consts::TAU;

/// Mono at 22.05 kHz, not the 44.1 kHz stereo of converted sounds: this is a
/// third of a second of two sine waves and it is embedded in the binary, so
/// the smallest format that carries it faithfully is the right one. 0.32 s
/// comes to about 14 KB.
const RATE: u32 = 22_050;
const SECONDS: f32 = 0.32;

/// A perfect fifth, A5 over E6. Two partials rather than one because a single
/// sine reads as a test tone; two read as a chime.
const LOW_HZ: f32 = 880.0;
const HIGH_HZ: f32 = 1318.5;

fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: make-chime <output.wav>")
    })?;

    let total = (RATE as f32 * SECONDS) as usize;
    let mut pcm = Vec::with_capacity(total * 2);
    for n in 0..total {
        let t = n as f32 / RATE as f32;
        // A 5 ms ramp in. Starting at full amplitude puts a step edge at sample
        // zero, which is audible as a click in front of the chime.
        let attack = (t / 0.005).min(1.0);
        let decay = (-t * 9.0).exp();
        let tone = (TAU * LOW_HZ * t).sin() * 0.6 + (TAU * HIGH_HZ * t).sin() * 0.4;
        // Peak amplitude is 0.6 + 0.4 = 1.0 before this halving, so the result
        // cannot clip even where the two partials align.
        let sample = tone * attack * decay * 0.5;
        pcm.extend_from_slice(&((sample * f32::from(i16::MAX)) as i16).to_le_bytes());
    }

    std::fs::write(&out, trix_core::sound::wav_from_pcm(&pcm, 1, RATE, 16))?;
    println!("wrote {out} ({} bytes)", 44 + pcm.len());
    Ok(())
}
```

- [ ] **Step 6: Generate the asset**

```bash
mkdir -p crates/trix-daemon/assets
cargo run -p trix-core --example make-chime -- crates/trix-daemon/assets/clip.wav
```

Expected: `wrote crates/trix-daemon/assets/clip.wav (14156 bytes)`.

- [ ] **Step 7: Listen to it**

Run: `powershell -c "(New-Object Media.SoundPlayer 'crates/trix-daemon/assets/clip.wav').PlaySync()"`
Expected: a short two-tone chime, no click at the start, no buzz. If it clicks, the attack ramp is wrong; if it is silent, the amplitude scaling is.

- [ ] **Step 8: Commit**

```bash
git add crates/trix-core/src/sound/ crates/trix-core/src/lib.rs crates/trix-core/examples/make-chime.rs crates/trix-daemon/assets/clip.wav
git commit -m "feat(sound): WAV container helpers and the built-in chime

PlaySound decodes WAV and nothing else, so every sound Trix plays goes
through this container. The header is written by hand -- fourteen
little-endian integers against a dependency, in a project whose pitch is
being small.

The chime has a generator rather than a provenance, so nobody has to wonder
where a binary blob came from or what licence it carries."
```

---

### Task 2: The two config keys

**Files:**
- Modify: `crates/trix-core/src/config.rs:86-124` (struct tail, `Default`, helpers)

**Interfaces:**
- Produces: `Config::clip_sound: bool` (default `true`), `Config::clip_sound_path: String` (default `""`), and `Config::sound_cache_path(config_path: &Path) -> PathBuf`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block at the end of `crates/trix-core/src/config.rs`:

```rust
/// `serde(default)` on a bool is `false`, so a config file written before
/// these keys existed would silently turn the sound off for every upgrading
/// user. `check_for_updates` learned this the same way.
#[test]
fn clip_sound_defaults_on_for_a_config_written_before_it_existed() {
    let old = "fps = 60\nbitrate_kbps = 8000\n";
    let config: Config = toml::from_str(old).expect("an old config must still parse");
    assert!(config.clip_sound, "a missing clip_sound must read as on");
    assert_eq!(config.clip_sound_path, "", "a missing path must read as the built-in");
}

#[test]
fn the_sound_keys_round_trip_through_toml() {
    let config = Config {
        clip_sound: false,
        clip_sound_path: r"C:\Users\someone\airhorn.mp3".into(),
        ..Config::default()
    };
    let round: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert!(!round.clip_sound);
    assert_eq!(round.clip_sound_path, r"C:\Users\someone\airhorn.mp3");
}

/// The cache is a sibling of the config file, which is what makes it scratch
/// in tests for free: a `Daemon` built with a scratch `config_path` gets a
/// scratch cache without a second seam to remember.
#[test]
fn the_sound_cache_sits_beside_the_config_file() {
    let cache = Config::sound_cache_path(std::path::Path::new(r"C:\x\trix\config.toml"));
    assert_eq!(cache, std::path::PathBuf::from(r"C:\x\trix\clip-sound.wav"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p trix-core config::`
Expected: FAIL — `no field 'clip_sound' on type 'Config'`.

- [ ] **Step 3: Add the fields**

In `crates/trix-core/src/config.rs`, after the `check_for_updates` field (line 94), inside the struct:

```rust
    /// Whether the daemon plays a sound when it saves a clip.
    ///
    /// Defaulted `true` for the same reason as `check_for_updates`: a config
    /// file written before this key existed must not read as "off".
    #[serde(default = "default_true")]
    pub clip_sound: bool,
    /// The sound file the user chose, or empty for Trix's built-in chime.
    ///
    /// This is the **original** — the mp3 they picked — and the daemon never
    /// plays it directly. `PlaySound` decodes only WAV, so the file is
    /// converted once when it is chosen and the converted copy is what plays.
    /// This value exists to be shown in settings and to be reconverted from.
    pub clip_sound_path: String,
```

In `impl Default for Config`, after `check_for_updates: true,`:

```rust
            clip_sound: true,
            clip_sound_path: String::new(),
```

- [ ] **Step 4: Add the cache-path helper**

In `impl Config`, after `pub fn path()`:

```rust
    /// Where the converted copy of the user's sound lives: beside the config
    /// file, so `%APPDATA%\trix\config.toml` gives `%APPDATA%\trix\clip-sound.wav`.
    ///
    /// Derived from the config path rather than from `%APPDATA%` directly, and
    /// that is the whole point: the daemon already carries a `config_path` that
    /// tests point at a scratch directory, so the cache follows it there
    /// without a second thing to remember to isolate.
    pub fn sound_cache_path(config_path: &Path) -> PathBuf {
        config_path.with_file_name("clip-sound.wav")
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p trix-core config::`
Expected: PASS.

- [ ] **Step 6: Check nothing else broke**

Run: `cargo test --workspace`
Expected: all pass. `deny_unknown_fields` means any test with a literal config table still parses, because both keys have defaults.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-core/src/config.rs
git commit -m "feat(sound): clip_sound and clip_sound_path config keys

clip_sound defaults on through default_true, not serde's false: a config
written before the key existed must not read as 'the user turned this off'.

The cache path is derived from config_path rather than %APPDATA% so it
follows the scratch path tests already set, instead of needing a second seam
nobody remembers to isolate."
```

---

### Task 3: Playing the sound on clip save

**Files:**
- Create: `crates/trix-daemon/src/sound.rs`
- Modify: `Cargo.toml` (workspace `windows` features)
- Modify: `crates/trix-daemon/src/lib.rs` (module list)
- Modify: `crates/trix-daemon/src/state.rs:620-638` (`record_saved_clip`)

**Interfaces:**
- Consumes: `trix_core::sound::looks_like_wav`, `Config::clip_sound`, `Config::clip_sound_path`, `Config::sound_cache_path`.
- Produces: `crate::sound::play(custom: Option<&Path>)`.

- [ ] **Step 1: Add the Windows feature**

In the workspace `Cargo.toml`, in `[workspace.dependencies.windows]`, add to the `features` list in alphabetical position (between `Win32_Graphics_Imaging` and `Win32_Media_MediaFoundation`):

```toml
    "Win32_Media_Audio",
```

- [ ] **Step 2: Write the failing test**

Create `crates/trix-daemon/src/sound.rs` with the doc comment, the asset, and the tests:

```rust
//! The sound played when a clip is saved.

#[cfg(test)]
mod tests {
    use super::*;

    /// The asset is embedded, so a corrupted or truncated commit of it would
    /// otherwise ship and simply never play. This is the only check that
    /// exists between the file in the repo and a silent release.
    #[test]
    fn the_built_in_chime_is_a_well_formed_wav() {
        assert!(trix_core::sound::looks_like_wav(BUILT_IN), "assets/clip.wav is not a WAV");
        assert!(BUILT_IN.len() > 1_000, "assets/clip.wav is suspiciously small");
        assert!(BUILT_IN.len() < 64 * 1024, "assets/clip.wav is too big to embed");
    }

    /// `play` must not panic when the cache it is handed is absent -- that is
    /// the ordinary state on a fresh install, and on the failure path where a
    /// conversion never landed. It falls back to the built-in.
    #[test]
    fn a_missing_cache_file_is_not_a_panic() {
        play(Some(std::path::Path::new(r"Z:\definitely\not\here.wav")));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Register the module first. In `crates/trix-daemon/src/lib.rs`, add `pub mod sound;` in alphabetical position among the existing `pub mod` lines.

Run: `cargo test -p trix-daemon sound::`
Expected: FAIL — `cannot find value 'BUILT_IN' in this scope`.

- [ ] **Step 4: Write the implementation**

Put this above the `#[cfg(test)] mod tests` block in `crates/trix-daemon/src/sound.rs`:

```rust
use std::path::Path;

use windows::Win32::Media::Audio::{
    PlaySoundW, SND_ASYNC, SND_FILENAME, SND_MEMORY, SND_NODEFAULT,
};
use windows::core::{HSTRING, PCWSTR};

/// Trix's own chime, generated by `crates/trix-core/examples/make-chime.rs`.
///
/// Embedded rather than shipped as a sixth file in the release zip, which
/// would put it in the updater's `BINARIES`/`DOCS` lists and the
/// `SHA256SUMS.txt` flow for no gain. Being `'static` also happens to be what
/// makes `SND_MEMORY | SND_ASYNC` sound: that pair requires the buffer to
/// outlive playback, and a `'static` slice satisfies it by construction rather
/// than by careful lifetime management.
const BUILT_IN: &[u8] = include_bytes!("../assets/clip.wav");

/// Plays `custom` if it is there, and Trix's own chime otherwise.
///
/// Never returns an error, and that is deliberate: the only caller is the clip
/// path, where the clip is already written and broadcast by the time this runs.
/// A sound is feedback about a thing that already happened, so nothing here is
/// allowed to become the reason a clip reports failure.
///
/// `SND_ASYNC` returns as soon as playback is queued rather than when it
/// finishes, so this costs the clip path microseconds, not the length of the
/// sound. `SND_NODEFAULT` is what stops Windows substituting its own ding when
/// the file cannot be played -- without it, a broken sound is indistinguishable
/// from a working one to anybody debugging this.
pub fn play(custom: Option<&Path>) {
    let ok = match custom.filter(|path| path.exists()) {
        Some(path) => unsafe {
            PlaySoundW(&HSTRING::from(path), None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT)
        },
        None => unsafe {
            PlaySoundW(
                PCWSTR(BUILT_IN.as_ptr().cast()),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            )
        },
    };
    if !ok.as_bool() {
        tracing::warn!(sound = ?custom, "the clip sound could not be played");
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p trix-daemon sound::`
Expected: PASS, 2 tests. The second plays the built-in chime on the developer's machine — that is intended, and is the only audible test in the suite.

- [ ] **Step 6: Hook it into the clip path**

In `crates/trix-daemon/src/state.rs`, replace the body of `record_saved_clip` (currently lines 620-638) with:

```rust
    fn record_saved_clip(&self, meta: &ClipMeta) {
        // Read before the library lock is taken, not after. `config` is only
        // ever held alone or under `armed` (see the lock ordering above), and
        // taking it after `library` here would introduce the first place in
        // the daemon where those two are held the other way round.
        let sound = {
            let config = self.lock_config();
            if !config.clip_sound {
                None
            } else if config.clip_sound_path.trim().is_empty() {
                // The built-in chime. `play` takes `None` to mean that.
                Some(None)
            } else {
                Some(self.config_path.as_deref().map(Config::sound_cache_path))
            }
        };

        // Prepended, matching `library::scan`'s newest-first order. This is
        // the incremental update that keeps `library.list` off the disk
        // after startup (spec §5.2).
        self.lock_library().insert(0, meta.clone());
        self.enforce_library_ceiling();
        // After the insert, so a client that answers the event by calling
        // `library.list` cannot race ahead of the clip it was told about.
        match serde_json::to_value(meta) {
            Ok(data) => self.clients.broadcast(&Event::new("clip_saved", data)),
            // `ClipMeta` is strings, integers and bools, so this cannot
            // actually happen -- but `panic = "abort"` leaves no room for
            // an `unwrap` on a path the hotkey reaches, and a clip that
            // saved is still saved even if nobody can be told.
            Err(e) => {
                tracing::error!(error = %e, "could not serialize a saved clip for clip_saved");
            }
        }

        // Last, after the clip is on disk and every client has been told. The
        // sound is feedback about a completed clip, so it goes behind
        // everything that makes the clip real.
        if let Some(custom) = sound {
            crate::sound::play(custom.as_deref());
        }
    }
```

Ensure `Config` is in scope in `state.rs` — it already is, via the existing `use trix_core::config::Config;`.

- [ ] **Step 7: Test the hook**

Add to `crates/trix-daemon/src/state.rs`'s test module:

```rust
/// The sound must not be able to break the announcement. This drives the same
/// function the hotkey reaches, with the sound turned on, and asserts the
/// event still arrives -- the ordering the comment in `record_saved_clip`
/// promises is otherwise only a comment.
///
/// `idle` supplies the scratch `clip_dir`, and `config_path` is `None`, so
/// nothing here can reach the developer's real config or clip folder.
#[test]
fn a_clip_is_still_announced_when_the_sound_is_on() {
    use std::sync::mpsc::sync_channel;

    let daemon = idle("clip-sound-on", Config { clip_sound: true, ..Config::default() });
    let (tx, rx) = sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
    daemon.clients.register(tx);

    daemon.record_saved_clip(&meta("20260817_120000"));

    let line = rx.try_recv().expect("a saved clip must still broadcast clip_saved");
    assert!(line.contains("clip_saved"), "the sound must not displace the event: {line}");
}
```

The helpers are the module's own: `idle(name, config)` at `state.rs:1292` (it supplies the scratch `clip_dir`), `meta(id)` at `state.rs:1244`, and the `sync_channel` + `clients.register(tx)` pair used by `a_recorded_clip_is_announced_to_every_client` at `state.rs:1729`. Note `state.rs`'s `idle` takes **two** arguments, unlike `dispatch.rs`'s.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p trix-daemon`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/trix-daemon/src/sound.rs crates/trix-daemon/src/lib.rs crates/trix-daemon/src/state.rs
git commit -m "feat(sound): play a chime when a clip is saved

PlaySound with SND_ASYNC, so the clip path pays microseconds rather than the
length of the sound, and SND_NODEFAULT, so a broken sound is silent instead
of indistinguishable from a working one.

Config is read before the library lock rather than after: config is only ever
held alone or under armed, and taking it after library would have been the
first place those two were held the other way round."
```

---

### Task 4: Decoding a user's file

**Files:**
- Modify: `crates/trix-core/src/sound/decode.rs` (created empty in Task 1)

**Interfaces:**
- Consumes: `trix_core::sound::{SAMPLE_RATE, CHANNELS, BITS_PER_SAMPLE, MAX_PCM_BYTES, capped, wav_from_pcm}`.
- Produces: `trix_core::sound::decode::to_wav(path: &Path) -> anyhow::Result<Vec<u8>>`.

- [ ] **Step 1: Write the tests**

Replace the contents of `crates/trix-core/src/sound/decode.rs` with the tests only:

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p trix-core sound::decode`
Expected: FAIL — `cannot find function 'to_wav' in this scope`.

- [ ] **Step 3: Write the implementation**

Put this above the test module in `crates/trix-core/src/sound/decode.rs`:

```rust
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

/// Media Foundation started for the duration of a call, and stopped on the way
/// out however the call ends.
///
/// A guard rather than a pair of calls because every `?` between them would
/// otherwise leak an MF startup count. `probe.rs` does the same thing by hand
/// in one function; this one has a dozen fallible steps.
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
    // decoder and resampler the source needs to reach this format, so an mp3 at
    // 48 kHz mono arrives as 44.1 kHz stereo 16-bit without anything below
    // knowing it was ever an mp3.
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
    loop {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        unsafe {
            reader.ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                None,
                Some(&mut sample),
            )
        }
        .context("reading decoded audio failed")?;

        if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
            break;
        }
        // A read can legitimately return no sample -- a format change or a gap
        // -- without being the end of the stream. Skipping is correct; treating
        // it as the end would truncate the sound at the first hiccup.
        let Some(sample) = sample else { continue };

        let buffer: IMFMediaBuffer =
            unsafe { sample.ConvertToContiguousBuffer() }.context("ConvertToContiguousBuffer")?;
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut length = 0u32;
        unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }.context("IMFMediaBuffer::Lock")?;
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trix-core sound::`
Expected: PASS. The ignored test is listed as ignored.

- [ ] **Step 5: Run the ignored test by hand**

Run: `cargo test -p trix-core sound::decode -- --ignored --nocapture`
Expected: PASS. If it fails with an MF error, the media type is wrong — check the five `SetUINT32`/`SetGUID` calls against the constants in Task 1.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-core/src/sound/decode.rs
git commit -m "feat(sound): decode any Windows-supported audio to capped WAV

Media Foundation is already linked for encoding, so mp3, m4a, wma and flac
arrive for the price of a feature flag. IMFSourceReader is asked for 16-bit
PCM directly, which pushes the resampling and channel conversion into MF
instead of into this file.

Reading stops at the cap rather than decoding a whole album and discarding
it, and a sample-less read is skipped rather than treated as the end -- a
format change mid-file would otherwise truncate the sound at the first
hiccup."
```

---

### Task 5: Converting on `config.set`, and repairing on startup

**Files:**
- Modify: `crates/trix-daemon/src/state.rs:758-894` (`set_config`)
- Modify: `crates/trix-daemon/src/main.rs` (startup repair)

**Interfaces:**
- Consumes: `trix_core::sound::decode::to_wav`, `Config::sound_cache_path`.
- Produces: `Daemon::repair_sound_cache(&self)` — public so `main.rs` can call it at startup.

- [ ] **Step 1: Write the failing tests**

Add to `crates/trix-daemon/src/state.rs`'s test module. These use the existing `with_scratch_config` seam so nothing touches the developer's real config:

```rust
/// The gate must refuse before anything is written, like every other gate in
/// `set_config`. A user who picks a text file must end up with the sound they
/// had, not with a config pointing at something that will never play.
#[test]
fn a_file_that_is_not_audio_is_refused_and_changes_nothing() {
    let (daemon, config_path, dir) = with_scratch_config("sound-refused");
    let bogus = dir.join("not-audio.mp3");
    std::fs::write(&bogus, b"this is text, not audio").unwrap();

    let mut values = Map::new();
    values.insert("clip_sound_path".into(), Value::from(bogus.to_string_lossy().as_ref()));
    let refused = daemon.set_config(&values);

    assert!(refused.is_err(), "a text file must not be accepted as a sound");
    let message = format!("{:#}", refused.unwrap_err());
    assert!(message.contains("not-audio.mp3"), "the error must name the file: {message}");
    assert_eq!(daemon.lock_config().clip_sound_path, "", "the old value must still stand");
    assert!(
        !Config::sound_cache_path(&config_path).exists(),
        "a refused sound must not leave a cache behind"
    );
}

/// Clearing the key is "Reset to default", and the cache has to go with it --
/// otherwise the next play finds a stale file beside a config that says
/// "built-in" and plays the sound the user just removed.
#[test]
fn clearing_the_path_deletes_the_cache() {
    let (daemon, config_path, _dir) = with_scratch_config("sound-reset");
    let cache = Config::sound_cache_path(&config_path);
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, b"RIFF____WAVEstale").unwrap();

    let mut values = Map::new();
    values.insert("clip_sound_path".into(), Value::from(""));
    daemon.set_config(&values).expect("clearing the sound must be accepted");

    assert!(!cache.exists(), "Reset must remove the converted copy");
}

/// A missing path is refused by name rather than by codec error, because that
/// is the mistake people actually make.
#[test]
fn a_path_that_does_not_exist_is_refused() {
    let (daemon, _config_path, _dir) = with_scratch_config("sound-missing");
    let mut values = Map::new();
    values.insert("clip_sound_path".into(), Value::from(r"Z:\nope\ghost.mp3"));

    let message = format!("{:#}", daemon.set_config(&values).unwrap_err());
    assert!(message.contains("ghost.mp3"), "the error must name the file: {message}");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p trix-daemon sound`
Expected: FAIL — the first test fails because a bogus path is currently accepted.

- [ ] **Step 3: Add the conversion gate**

In `crates/trix-daemon/src/state.rs`, immediately after the `clip_dir` gate (currently lines 827-829), add:

```rust
        // Decoded here, before anything is written, for the same all-or-nothing
        // reason as the gate above: a file that is not audio must leave the
        // user with the sound they already had. The bytes are held rather than
        // written because the config file has not been saved yet -- a new cache
        // beside an unchanged config is exactly the mismatch this ordering
        // exists to prevent.
        //
        // Guarded on the key being present, so five unrelated keys do not start
        // Media Foundation to save a bitrate.
        let converted = if values.contains_key("clip_sound_path") {
            let chosen = updated.clip_sound_path.trim();
            if chosen.is_empty() {
                // Reset: nothing to convert, and the cache must go.
                Some(None)
            } else {
                Some(Some(
                    trix_core::sound::decode::to_wav(std::path::Path::new(chosen))
                        .context("that file cannot be used as a sound")?,
                ))
            }
        } else {
            None
        };
```

- [ ] **Step 4: Write the cache after the config saves**

In the same function, immediately after `updated.save_to(path).context("config.set could not save the config")?;`, add:

```rust
        // After the config is on disk, not before. If this fails the config is
        // still correct and `repair_sound_cache` rebuilds the cache at the next
        // startup, which is a far smaller hole than a cache that plays a sound
        // the saved config does not name.
        match converted {
            Some(Some(wav)) => {
                if let Err(e) = write_sound_cache(path, &wav) {
                    tracing::warn!(error = %format!("{e:#}"), "could not save the converted sound");
                }
            }
            Some(None) => {
                // Best-effort: there is usually no cache to remove, because the
                // user was on the built-in sound already.
                let _ = std::fs::remove_file(Config::sound_cache_path(path));
            }
            None => {}
        }
```

Then add this free function near the other file helpers at the end of `state.rs`, outside the `impl`:

```rust
/// Writes the converted sound beside the config file, through a temporary name.
///
/// Temp-then-rename rather than a direct write: a half-written cache is a valid
/// path with an invalid file at it, and `PlaySound` answers that with silence.
/// A rename on the same volume is atomic, so the cache is either the old sound
/// or the new one and never a prefix of either.
fn write_sound_cache(config_path: &std::path::Path, wav: &[u8]) -> Result<()> {
    let final_path = Config::sound_cache_path(config_path);
    let temp_path = final_path.with_extension("wav.tmp");
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(&temp_path, wav)
        .with_context(|| format!("could not write {}", temp_path.display()))?;
    std::fs::rename(&temp_path, &final_path)
        .with_context(|| format!("could not replace {}", final_path.display()))?;
    Ok(())
}
```

- [ ] **Step 5: Add the startup repair**

Add this method to `impl Daemon` in `crates/trix-daemon/src/state.rs`, next to the other public methods:

```rust
    /// Rebuilds the converted sound if it is missing.
    ///
    /// Two things reach this state and both are ordinary: a `config.toml`
    /// edited by hand, which names a sound no conversion ever ran for, and a
    /// cache someone deleted while tidying `%APPDATA%`.
    ///
    /// Called at startup rather than from the constructor so no test builds a
    /// `Daemon` that starts Media Foundation.
    pub fn repair_sound_cache(&self) {
        let Some(config_path) = self.config_path.as_deref() else {
            return;
        };
        let chosen = self.lock_config().clip_sound_path.trim().to_string();
        if chosen.is_empty() || Config::sound_cache_path(config_path).exists() {
            return;
        }

        match trix_core::sound::decode::to_wav(std::path::Path::new(&chosen)) {
            Ok(wav) => {
                if let Err(e) = write_sound_cache(config_path, &wav) {
                    tracing::warn!(error = %format!("{e:#}"), "could not rebuild the clip sound");
                }
            }
            // The setting is deliberately left alone. Putting the file back and
            // restarting is then all it takes; clearing it here would quietly
            // discard a choice the user still wants.
            Err(e) => tracing::warn!(
                error = %format!("{e:#}"),
                sound = %chosen,
                "the chosen clip sound could not be read; using the built-in one"
            ),
        }
    }
```

- [ ] **Step 6: Call it at startup**

In `crates/trix-daemon/src/main.rs`, after `let daemon = Arc::new(Daemon::new(config));` (line 190), add:

```rust
    // After the daemon exists and before the socket opens: a hand-edited
    // config or a deleted cache is repaired before the first clip can need it.
    daemon.repair_sound_cache();
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p trix-daemon`
Expected: PASS, including the three new tests.

- [ ] **Step 8: Commit**

```bash
git add crates/trix-daemon/src/state.rs crates/trix-daemon/src/main.rs
git commit -m "feat(sound): convert the chosen file in the config.set gate

Decoding is the validation: a file that decodes is accepted, one that does
not is refused with Windows' own reason. No extension whitelist, because the
extension was never the thing that mattered.

The decode runs before any write and the cache lands after the config saves,
so a refusal leaves the previous sound in force and a failed cache write
leaves a config that startup can repair -- rather than a cache playing a
sound the saved config does not name."
```

---

### Task 6: `sound.pick` and `sound.test`

**Files:**
- Modify: `crates/trix-proto/src/command.rs:15-78`
- Modify: `crates/trix-daemon/src/folder.rs`
- Modify: `crates/trix-daemon/src/window.rs`
- Modify: `crates/trix-daemon/src/dispatch.rs`

**Interfaces:**
- Consumes: `crate::sound::play`, `Config::sound_cache_path`, `Daemon::set_config`.
- Produces: `Command::SoundPick`, `Command::SoundTest`, `folder::pick_sound()`, `window::request_sound_pick()`.

- [ ] **Step 1: Write the failing protocol test**

Add to `crates/trix-proto/src/command.rs`'s test module:

```rust
#[test]
fn the_sound_commands_parse() {
    assert_eq!(parse(r#"{"id":1,"cmd":"sound.pick"}"#).1, Ok(Command::SoundPick));
    assert_eq!(parse(r#"{"id":2,"cmd":"sound.test"}"#).1, Ok(Command::SoundTest));
}
```

`parse` is the module's own helper at the top of that test module: `fn parse(line: &str) -> (u64, Result<Command, String>)`, so `.1` is the command.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p trix-proto`
Expected: FAIL — `no variant named 'SoundPick'`.

- [ ] **Step 3: Add the variants**

In `crates/trix-proto/src/command.rs`, add to the `Command` enum after `StatsSubscribe`:

```rust
    /// Opens the "choose a sound" dialog. Answers immediately: the dialog
    /// outlives the request by as long as the user takes to browse, and the
    /// result arrives as a `config_changed` event, not as this reply.
    SoundPick,
    /// Plays the configured clip sound once, so the user can hear what they
    /// just chose without saving a clip.
    SoundTest,
```

And to `parse`, after the `stats.subscribe` arm:

```rust
            "sound.pick" => Self::SoundPick,
            "sound.test" => Self::SoundTest,
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p trix-proto`
Expected: PASS.

- [ ] **Step 5: Add the audio-file dialog**

First, the feature this needs. `COMDLG_FILTERSPEC` lives in `Win32::UI::Shell::Common`, which is a **separate feature** from `Win32_UI_Shell` and is not currently enabled. In the workspace `Cargo.toml`, in `[workspace.dependencies.windows]`, add immediately after `"Win32_UI_Shell",`:

```toml
    "Win32_UI_Shell_Common",
```

Then, in `crates/trix-daemon/src/folder.rs`, change the module doc's first line to:

```rust
//! The daemon's file dialogs — "Change clips folder…" and "choose a clip
//! sound" — and the one message box that reports them failing.
```

Extend the imports:

```rust
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog,
    IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
```

`COMDLG_FILTERSPEC` is imported from `Shell::Common`, not `Shell` — a detail that costs a confusing "unresolved import" if guessed.

Extract the result-reading tail of `show` into a shared helper — replace the last six lines of `show` (from `let item = unsafe { dialog.GetResult() }` to the end) with `chosen_path(&dialog)`, and add:

```rust
/// Pulls the chosen filesystem path out of a dialog the user accepted.
///
/// Shared by both dialogs because the shell's ownership rule is the part worth
/// writing once: `GetDisplayName` hands back memory the shell allocated, and it
/// is ours to free whatever else happens — so the string is copied out before
/// anything can return early.
fn chosen_path(dialog: &IFileOpenDialog) -> Result<Option<PathBuf>> {
    let item = unsafe { dialog.GetResult() }.context("IFileOpenDialog::GetResult")?;
    let wide = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
        .context("the chosen item has no filesystem path")?;
    let chosen = unsafe { wide.to_string() };
    unsafe { CoTaskMemFree(Some(wide.0 as *const _)) };
    Ok(Some(PathBuf::from(chosen.context("the chosen path is not valid UTF-16")?)))
}

/// Shows the "choose a clip sound" dialog.
///
/// `Ok(None)` is a cancel — a normal outcome the caller must not report.
///
/// On its own thread with its own apartment for exactly the reasons [`pick`]
/// documents at length: the shell's dialogs are apartment-threaded, and a
/// caller that has already run Media Foundation may be in an MTA, which is how
/// pickers end up invisible or behind the game.
pub fn pick_sound() -> Result<Option<PathBuf>> {
    std::thread::Builder::new()
        .name("trix-sound-picker".into())
        .spawn(show_sound)
        .context("could not start the sound-picker thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("the sound-picker thread panicked"))?
}

fn show_sound() -> Result<Option<PathBuf>> {
    let _apartment = Apartment::enter()?;

    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
            .context("could not create the sound picker")?;

    let options = unsafe { dialog.GetOptions() }.context("IFileDialog::GetOptions")?;
    unsafe { dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST) }
        .context("IFileDialog::SetOptions")?;
    let _ = unsafe { dialog.SetTitle(&HSTRING::from("Choose the sound Trix plays for a clip")) };

    // The filter is a convenience, not the rule -- the decoder is what actually
    // decides. "All files" is second so somebody with an .opus or an .aiff can
    // still try it, and find out from the error rather than from a dialog that
    // refuses to show them their own file.
    let audio = HSTRING::from("Audio files");
    let audio_spec = HSTRING::from("*.mp3;*.wav;*.m4a;*.wma;*.flac");
    let all = HSTRING::from("All files");
    let all_spec = HSTRING::from("*.*");
    let filters = [
        COMDLG_FILTERSPEC { pszName: PCWSTR(audio.as_ptr()), pszSpec: PCWSTR(audio_spec.as_ptr()) },
        COMDLG_FILTERSPEC { pszName: PCWSTR(all.as_ptr()), pszSpec: PCWSTR(all_spec.as_ptr()) },
    ];
    let _ = unsafe { dialog.SetFileTypes(&filters) };

    if let Err(e) = unsafe { dialog.Show(None) } {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            return Ok(None);
        }
        return Err(anyhow::Error::from(e).context("the sound picker failed"));
    }

    chosen_path(&dialog)
}
```

Add `use windows::core::PCWSTR;` to the imports at the top.

- [ ] **Step 6: Add the window action**

In `crates/trix-daemon/src/window.rs`, add to `enum Action` after `ChangeClipsFolder` (or wherever the folder variant sits):

```rust
    /// The settings page asked for the "choose a clip sound" dialog. Handled
    /// on a thread of its own rather than on the worker, so the clip hotkey
    /// keeps working while the dialog is open.
    PickClipSound,
```

Add the message constant beside the other `WM_TRIX_*` constants:

```rust
pub(crate) const WM_TRIX_PICK_SOUND: u32 = WM_APP + 0x13;
```

`0x13` is the next free number: `window.rs` uses `0x10`, `0x11` and `0x12`, and `tray.rs` uses `WM_APP + 1`.

Add the request function next to `rebind_hotkey`:

```rust
/// Asks the pump for the "choose a clip sound" dialog. Returns immediately.
///
/// A no-op when there is no pump — unit tests, and `trix.exe`. The `cfg!(test)`
/// guard is the same one `rebind_hotkey` carries and for the same reason:
/// `WINDOW_HWND` is process-global, and a test in this crate could otherwise
/// read a live pump another test is running and open a real file dialog on the
/// developer's desktop.
pub fn request_sound_pick() {
    if cfg!(test) {
        return;
    }
    let hwnd = WINDOW_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            WM_TRIX_PICK_SOUND,
            WPARAM(0),
            LPARAM(0),
        );
    }
}
```

In the window procedure, add an arm beside the other `WM_TRIX_*` cases (after the `WM_TRIX_REHOTKEY` arm, which ends around line 361). It is the same `ACTIONS.with` / `offer` shape the `WM_HOTKEY` and rehotkey arms use:

```rust
        WM_TRIX_PICK_SOUND => {
            // The pump only forwards. Running a modal dialog here would stop
            // the tray icon answering for as long as the user browses, which
            // is the failure `folder.rs` documents at length.
            ACTIONS.with(|a| {
                if let Some(tx) = a.borrow().as_ref() {
                    offer(tx, Action::PickClipSound);
                }
            });
            LRESULT(0)
        }
```

- [ ] **Step 7: Handle the action**

In `handle_action` in `crates/trix-daemon/src/window.rs`, add:

```rust
        Action::PickClipSound => choose_clip_sound(daemon),
```

And add, next to `change_clips_folder`:

```rust
/// Runs the sound dialog and applies the result.
///
/// **On a detached thread, unlike `change_clips_folder`.** The worker thread
/// also handles `Action::Clip`, so a modal dialog held open on it means the
/// clip hotkey does nothing until the user finishes browsing. The folder picker
/// has always had that flaw; there is no reason to copy it. `Arc<Daemon>` is
/// already in hand here, so the thread costs nothing but a clone.
///
/// The guard makes a second click while a dialog is open a no-op rather than a
/// second dialog.
fn choose_clip_sound(daemon: &Arc<Daemon>) {
    static PICKING: AtomicBool = AtomicBool::new(false);
    if PICKING.swap(true, Ordering::SeqCst) {
        return;
    }

    let daemon = Arc::clone(daemon);
    let spawned = std::thread::Builder::new().name("trix-sound-dialog".into()).spawn(move || {
        let chosen = crate::folder::pick_sound();
        PICKING.store(false, Ordering::SeqCst);

        let chosen = match chosen {
            Ok(Some(path)) => path,
            // Cancelled, and deliberately silent: answering a dialog the user
            // dismissed on purpose with a message box is what makes people stop
            // opening menus.
            Ok(None) => return,
            Err(e) => {
                let detail = format!("{e:#}");
                tracing::warn!(error = %detail, "the sound picker failed");
                crate::folder::report_error(&format!("Could not open the sound picker.\n\n{detail}"));
                return;
            }
        };

        let mut values = serde_json::Map::new();
        values.insert(
            "clip_sound_path".to_string(),
            serde_json::Value::from(chosen.to_string_lossy().as_ref()),
        );
        // Through the dispatch helper, not `set_config` directly: the settings
        // page is open in another process and learns about this only from the
        // `config_changed` broadcast that helper sends.
        match crate::dispatch::apply_config_and_broadcast(&daemon, &values) {
            // `_` because the helper returns the `ConfigUpdate`; nothing here
            // needs it, and `Ok(())` would not typecheck.
            Ok(_) => tracing::info!(sound = %chosen.display(), "clip sound changed"),
            Err(e) => {
                crate::folder::report_error(&format!("That sound can't be used:\n\n{e}"));
            }
        }
    });

    if spawned.is_err() {
        PICKING.store(false, Ordering::SeqCst);
        tracing::warn!("could not start the sound-dialog thread");
    }
}
```

Ensure `AtomicBool` and `Ordering` are already imported in `window.rs` — they are, for `WINDOW_HWND` and `finished`.

- [ ] **Step 8: Extract the broadcast helper**

In `crates/trix-daemon/src/dispatch.rs`, the existing `config_set` function builds the `changed` map and broadcasts at line 128-133. Pull that into a function both callers can use, keeping every existing comment with the code it explains:

```rust
/// Applies a `config.set` and tells every client what landed.
///
/// The broadcast belongs with the apply, not with the socket command: the tray
/// and the sound dialog change config too, and a client that learns about some
/// changes and not others is worse than one that learns about none.
pub(crate) fn apply_config_and_broadcast(
    daemon: &Daemon,
    values: &Map<String, Value>,
) -> Result<ConfigUpdate, String> {
    let update = daemon.set_config(values).map_err(|e| format!("{e:#}"))?;

    let mut changed = update.accepted.clone();
    changed.insert(
        "clip_dir_resolved".to_string(),
        Value::from(update.clip_dir_resolved.as_str()),
    );
    daemon.clients.broadcast(&Event::new("config_changed", Value::Object(changed)));

    Ok(update)
}
```

Then rewrite `config_set` (`dispatch.rs:109-142`) to call it. The response shape is unchanged — the UI reads both `accepted` and `requires_rearm`:

```rust
/// `{"accepted":{…},"requires_rearm":[…]}` (spec §4.3).
fn config_set(daemon: &Daemon, id: u64, values: &Map<String, Value>) -> Response {
    match apply_config_and_broadcast(daemon, values) {
        Ok(update) => {
            let mut fields = Map::new();
            fields.insert("accepted".to_string(), Value::Object(update.accepted));
            fields.insert(
                "requires_rearm".to_string(),
                Value::Array(update.requires_rearm.into_iter().map(Value::from).collect()),
            );
            Response::ok(id, Value::Object(fields))
        }
        // No `error` event. Spec §4.4 broadcasts one for `arm` and `clip`,
        // whose failure changes what every other client can expect to happen
        // next; a refused settings write concerns only the client that sent it.
        Err(e) => Response::err(id, e),
    }
}
```

**Move, do not delete, the two comments now in `config_set`.** The "Broadcast before the response" paragraph and the "Every accepted key, not just `clip_dir`" paragraph (`dispatch.rs:112-127`) explain the broadcast, so they move into `apply_config_and_broadcast` with it. The existing tail comment about there being no `error` event explains the `Err` arm and stays here.

Keep the existing error formatting identical: `set_config` returns `anyhow::Error` and the old code formatted it with `{e:#}`, which is why the helper does that and returns `Result<ConfigUpdate, String>`.

**Out of scope, do not change:** `window.rs`'s `change_clips_folder` still calls `daemon.set_config` directly and therefore still does not broadcast. That is a pre-existing gap this task deliberately leaves alone; it is noted so a reviewer knows it was seen, not missed.

- [ ] **Step 9: Add the dispatch arms**

In `crates/trix-daemon/src/dispatch.rs`, before the `Err(error)` arm:

```rust
            // Answers now, not when the dialog closes. A modal dialog lasts as
            // long as a person takes to browse, and a socket command that
            // blocked for that long would sit on this connection's reader while
            // the settings page waited on a reply that is not the answer
            // anyway -- the answer arrives as `config_changed`.
            Ok(Command::SoundPick) => {
                crate::window::request_sound_pick();
                Response::ok(request.id, Value::Object(Map::new()))
            }
            Ok(Command::SoundTest) => {
                let config = self.lock_config();
                let custom = if config.clip_sound_path.trim().is_empty() {
                    None
                } else {
                    self.config_path.as_deref().map(Config::sound_cache_path)
                };
                drop(config);
                // Plays whatever is configured, whether or not `clip_sound` is
                // on: this answers "what does this file sound like", and the
                // toggle is a separate question the settings page already shows.
                crate::sound::play(custom.as_deref());
                Response::ok(request.id, Value::Object(Map::new()))
            }
```

- [ ] **Step 10: Test the dispatch arms**

Add to `crates/trix-daemon/src/dispatch.rs`'s test module:

```rust
/// `sound.pick` must answer immediately rather than when a dialog closes.
/// `request_sound_pick` is a no-op under `cfg!(test)` -- see its comment --
/// so this asserts the reply without any dialog ever existing, which is also
/// what stops `cargo test` opening a file dialog on the developer's desktop.
#[test]
fn sound_pick_answers_immediately() {
    let response = idle("sound-pick").dispatch(1, &request(1, "sound.pick"));
    assert!(response.ok, "sound.pick must succeed: {:?}", response.error);
    assert_eq!(response.id, 1);
}

/// Plays the built-in chime, because `idle` leaves `clip_sound_path` empty.
/// Audible when the suite runs, and that is the point: a `sound.test` that
/// answered `ok` without a sound would pass a silent assertion too.
#[test]
fn sound_test_answers_ok() {
    let response = idle("sound-test").dispatch(1, &request(2, "sound.test"));
    assert!(response.ok, "sound.test must succeed: {:?}", response.error);
}
```

These use the module's own helpers: `idle(name)` (one argument here, unlike `state.rs`'s), `request(id, cmd)` at `dispatch.rs:346`, and `dispatch(client, &request) -> Response` — whose fields are `.id`, `.ok`, `.error`, `.data`, as `dispatch.rs:425` shows.

- [ ] **Step 11: Run everything**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 12: Commit**

```bash
git add crates/trix-proto/src/command.rs crates/trix-daemon/src/folder.rs crates/trix-daemon/src/window.rs crates/trix-daemon/src/dispatch.rs
git commit -m "feat(sound): sound.pick and sound.test over the control socket

sound.pick answers immediately and the dialog runs on a thread of its own,
not on the worker: the worker also handles the clip hotkey, and the folder
picker's habit of blocking it for as long as a person takes to browse is a
flaw worth not copying.

The config broadcast moves out of the socket command and in with the apply,
so the settings page hears about a sound chosen in a dialog the same way it
hears about one typed into a field."
```

---

### Task 7: The settings row, and the docs

**Files:**
- Modify: `crates/trix-ui/web/src/lib/settings.ts:1-73`
- Modify: `crates/trix-ui/web/src/components/Field.svelte:96-111`
- Modify: `crates/trix-ui/web/src/views/Settings.svelte:29-37`
- Modify: `docs/ship/README.txt`
- Test: `crates/trix-ui/web/src/lib/settings.test.ts`

**Interfaces:**
- Consumes: socket commands `sound.pick`, `sound.test`, config keys `clip_sound`, `clip_sound_path`.

- [ ] **Step 1: Write the failing test**

Add to `crates/trix-ui/web/src/lib/settings.test.ts`:

```ts
import { FIELDS, SECTIONS } from './settings';

describe('the clip sound fields', () => {
  it('puts both rows in a section the page already renders', () => {
    // A field in a section `SECTIONS` does not list renders nowhere at all --
    // the settings page iterates `SECTIONS`, not `FIELDS`.
    for (const key of ['clip_sound', 'clip_sound_path']) {
      const field = FIELDS.find((f) => f.key === key);
      expect(field, `${key} must be a settings field`).toBeDefined();
      expect(SECTIONS).toContain(field!.section);
    }
  });

  it('renders the sound file row with the sound control', () => {
    expect(FIELDS.find((f) => f.key === 'clip_sound_path')!.kind).toBe('sound');
    expect(FIELDS.find((f) => f.key === 'clip_sound')!.kind).toBe('bool');
  });

  it('says which formats work, because PlaySound is not the whole story', () => {
    const help = FIELDS.find((f) => f.key === 'clip_sound_path')!.help;
    expect(help).toMatch(/mp3/i);
    expect(help).toMatch(/10 seconds/i);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd crates/trix-ui/web && npm test -- --run settings`
Expected: FAIL — `clip_sound must be a settings field`.

- [ ] **Step 3: Add the field kind and the rows**

In `crates/trix-ui/web/src/lib/settings.ts`, extend the kind union on line 1:

```ts
export type FieldKind = 'number' | 'text' | 'select' | 'bool' | 'folder' | 'hotkey' | 'slider' | 'sound';
```

Add two entries to `FIELDS`, in the `Trix` section, after the `clip_hotkey` row:

```ts
  { key: 'clip_sound', label: 'Clip sound', kind: 'bool', section: 'Trix', help: 'Plays a sound when a clip is saved, even when the Trix window is closed.' },
  { key: 'clip_sound_path', label: 'Sound file', kind: 'sound', section: 'Trix', help: 'Your own sound, or Trix\'s built-in one. mp3, wav, m4a and anything else Windows can play. Only the first 10 seconds are used.' },
```

- [ ] **Step 4: Render the row**

In `crates/trix-ui/web/src/components/Field.svelte`, add a branch after the `folder` branch (line 96-98):

```svelte
    {:else if field.kind === 'sound'}
      <input id={field.key} readonly disabled={!config['clip_sound']}
        value={String(config[field.key] ?? '') || "Trix's built-in sound"} />
      <button disabled={!config['clip_sound']} onclick={onpicksound}>Choose...</button>
      <button disabled={!config['clip_sound']} onclick={ontestsound}>Test</button>
      <button disabled={!config['clip_sound'] || !config[field.key]}
        onclick={() => onset(field.key, '')}>Reset</button>
```

Add the two callbacks to the props block (lines 18-40), beside the existing `on*` callbacks:

```ts
    onpicksound,
    ontestsound,
```

and to its type:

```ts
    onpicksound: () => void;
    ontestsound: () => void;
```

- [ ] **Step 5: Wire the page**

In `crates/trix-ui/web/src/views/Settings.svelte`, add the two handlers next to `set`:

```ts
  async function pickSound() {
    try {
      // Answers as soon as the dialog is open, not when it closes. The chosen
      // file arrives as a `config_changed` event, handled below.
      await call('sound.pick');
    } catch (e) {
      app.toast('error', String(e));
    }
  }

  async function testSound() {
    try {
      await call('sound.test');
    } catch (e) {
      app.toast('error', String(e));
    }
  }
```

Extend the existing `onDaemonEvent` handler (lines 29-34) with a `config_changed` case:

```ts
    if (event.event === 'config_changed') {
      // The sound dialog and the tray change config behind this page's back;
      // without this the path box would keep showing the old file until the
      // page was reopened.
      config = { ...config, ...event.data };
    }
```

Pass the callbacks down at the single `<Field ... />` render site, which starts at `Settings.svelte:147`. Add these two lines beside `onsavehotkey={saveHotkey}`:

```svelte
        onpicksound={pickSound}
        ontestsound={testSound}
```

- [ ] **Step 6: Run the frontend tests**

Run: `cd crates/trix-ui/web && npm test -- --run`
Expected: PASS, all suites.

Run: `cd crates/trix-ui/web && npx svelte-check --threshold error`
Expected: 0 errors, 0 warnings. A missing prop on `<Field>` shows up here, not in the tests.

- [ ] **Step 7: Document it**

In `docs/ship/README.txt`, add before `KNOWN LIMITS IN THIS RELEASE`:

```
CLIP SOUND
----------

Trix plays a short sound when it saves a clip, so you know the hotkey worked
without leaving your game. It plays whether or not the Trix window is open.

To use your own:  Settings -> Trix -> Sound file -> Choose...

mp3, wav, m4a and anything else Windows can play will work. Only the first
10 seconds are used. Trix converts your file once and keeps its own copy, so
moving or deleting the original later will not stop the sound.

Reset puts Trix's own sound back. To turn the sound off entirely, use the
Clip sound switch just above it.

There is no volume slider. Use Windows' volume mixer to set how loud
trix-daemon.exe is.
```

- [ ] **Step 8: Commit**

```bash
git add crates/trix-ui/web/src/lib/settings.ts crates/trix-ui/web/src/lib/settings.test.ts crates/trix-ui/web/src/components/Field.svelte crates/trix-ui/web/src/views/Settings.svelte docs/ship/README.txt
git commit -m "feat(ui): clip sound settings row

The path box shows the file the user chose and the daemon plays its own
converted copy, so the box reads \"Trix's built-in sound\" rather than empty
when there is no custom file -- an empty box looks like a setting that failed
to load.

Settings now consumes config_changed. The sound dialog runs in the daemon and
changes config behind this page's back, so without it the path box would keep
showing the old file until the page was reopened."
```

---

## Final verification

- [ ] `cargo test --workspace` passes, with no fewer tests than the 259 on `master`
- [ ] `cargo test -p trix-core sound::decode -- --ignored` passes (needs Media Foundation)
- [ ] `cargo clippy --workspace --all-targets 2>&1 | grep -c "^warning"` reports **no more than 14**

  14 is this repo's standing baseline, not zero: pre-existing lints in `trix-core` and `trix-daemon` plus the per-crate summary lines. The gate is "no increase". A `-D warnings` gate would fail on the first run against lints this branch did not introduce.
- [ ] `cd crates/trix-ui/web && npm test -- --run` passes
- [ ] `cd crates/trix-ui/web && npx svelte-check --threshold error` reports 0 errors, 0 warnings
- [ ] `rustfmt --edition 2024` has been run on every `.rs` file this plan touched — **not** `cargo fmt`
- [ ] Hand: build both binaries, take a clip with the hotkey **with the Trix window closed**, hear the chime
- [ ] Hand: Settings → Trix → Sound file → Choose…, pick an mp3, hear it with Test, take a clip, hear it
- [ ] Hand: delete the original mp3, take a clip, still hear it
- [ ] Hand: Reset, confirm the box reads "Trix's built-in sound" and `%APPDATA%\trix\clip-sound.wav` is gone
- [ ] Hand: turn Clip sound off, take a clip, hear nothing
- [ ] Hand: note whether the Choose… dialog opens **in front of** the Trix window. If it opens behind, the fix is one `AllowSetForegroundWindow(daemon_pid)` call from the UI before `sound.pick` — see the spec's Known limits. Do not build it pre-emptively.
