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
    let out =
        std::env::args().nth(1).ok_or_else(|| anyhow::anyhow!("usage: make-chime <output.wav>"))?;

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
