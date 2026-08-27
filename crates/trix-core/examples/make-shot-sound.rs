//! Generates the sound Trix plays when it saves a screenshot.
//!
//! Run once; its output is committed. It exists for the same reason
//! `make-chime.rs` does — the sound in the repo has a recipe rather than a
//! provenance nobody can check.
//!
//! Usage: cargo run -p trix-core --example make-shot-sound -- crates/trix-daemon/assets/shot.wav

use std::f32::consts::TAU;

/// Mono at 22.05 kHz, matching `make-chime.rs`: both are embedded in the
/// binary, so the smallest format that carries them faithfully is right.
const RATE: u32 = 22_050;

/// Half the clip chime's 0.32 s. The two are told apart by *length* before
/// pitch — a short blip against a ringing fifth is legible through gunfire in
/// a way two similar chimes are not.
const SECONDS: f32 = 0.16;

/// One partial, an octave above the clip chime's A5, and no interval. A single
/// sine reads as a camera blip rather than as a chime, which is the point: it
/// must not be mistaken for "a clip was saved".
const HZ: f32 = 1760.0;

fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: make-shot-sound <output.wav>"))?;

    let total = (RATE as f32 * SECONDS) as usize;
    let mut pcm = Vec::with_capacity(total * 2);
    for n in 0..total {
        let t = n as f32 / RATE as f32;
        // A 5 ms ramp in, for the same reason as the clip chime: full
        // amplitude at sample zero is a step edge, audible as a click.
        let attack = (t / 0.005).min(1.0);
        let decay = (-t * 18.0).exp();
        let sample = (TAU * HZ * t).sin() * attack * decay * 0.5;
        pcm.extend_from_slice(&((sample * f32::from(i16::MAX)) as i16).to_le_bytes());
    }

    std::fs::write(&out, trix_core::sound::wav_from_pcm(&pcm, 1, RATE, 16))?;
    println!("wrote {out} ({} bytes)", 44 + pcm.len());
    Ok(())
}
