//! Attribution harness for the per-cycle handle leak.
//!
//! `scripts/arm-cycle-leak.ps1` reports handles but does not gate them, and a
//! process-wide count cannot say *which* half of an arm leaks. These tests cycle
//! one subsystem at a time in-process and print the count after each cycle, so
//! the audio path and the full arm can be compared under identical timings.
//!
//! Both need real hardware (a render endpoint, a hardware H.264 encoder), so
//! both are ignored by default — same convention as the encoder tests in
//! `engine.rs`. Run one with:
//!     cargo test -p trix-core --release --test handle_leak -- --ignored --nocapture <name>

use std::time::Duration;

use trix_core::{
    capture::audio::{AudioCapture, AudioGains, AudioSourceKind},
    config::Config,
    engine::EngineHandle,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

const CYCLES: usize = 8;
const LIVE: Duration = Duration::from_secs(2);
const SETTLE: Duration = Duration::from_secs(3);

fn handles() -> u32 {
    let mut count = 0u32;
    unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }
        .expect("GetProcessHandleCount");
    count
}

/// Prints the per-cycle slope measured from cycle 1, not from process start:
/// the first cycle pays one-time costs (COM apartment, driver DLLs, endpoint
/// enumeration) that are not per-cycle and would otherwise be charged to the
/// leak. Same reasoning as the memory gate's "excluding the first".
fn report(label: &str, samples: &[u32]) {
    println!("\n[{label}]");
    for (i, n) in samples.iter().enumerate() {
        println!("  after cycle {:<2} handles {n}", i + 1);
    }
    let first = samples[0] as f64;
    let last = samples[samples.len() - 1] as f64;
    let span = (samples.len() - 1) as f64;
    println!("  {label} slope: {:.2} handles/cycle (excluding the first)", (last - first) / span);
}

#[test]
#[ignore = "needs a real render endpoint; run with --ignored"]
fn audio_only_cycles() {
    let mut samples = Vec::new();
    for _ in 0..CYCLES {
        let (capture, rx) =
            AudioCapture::start(AudioSourceKind::SystemAudio).expect("loopback start");
        std::thread::sleep(LIVE);
        capture.stop().expect("loopback stop");
        // Held until after stop: dropping the receiver first makes the capture
        // thread return early on a send error, which is a different teardown
        // path than the one an arm/disarm actually takes.
        drop(rx);
        std::thread::sleep(SETTLE);
        samples.push(handles());
    }
    report("audio only", &samples);
}

/// A WGC capture session with no encoder attached: `measure`'s handler only
/// counts frame arrivals. Splits the video half of an arm into "the capture
/// session" and "our encoder" — the two suspects left once audio is cleared.
#[test]
#[ignore = "needs a real display; run with --ignored"]
fn capture_only_cycles() {
    let mut samples = Vec::new();
    for _ in 0..CYCLES {
        // The result is deliberately discarded: `measure` reports "fewer than
        // two frames arrived" on a static desktop, and it stops the capture
        // before that check, so the session is created and torn down either
        // way — which is all this test is measuring.
        let _ = trix_core::capture::video::measure(0, LIVE.as_secs());
        std::thread::sleep(SETTLE);
        samples.push(handles());
    }
    report("capture only", &samples);
}

#[test]
#[ignore = "needs a real encoder; run with --ignored"]
fn full_arm_cycles() {
    let mut samples = Vec::new();
    for _ in 0..CYCLES {
        let engine = EngineHandle::spawn(Config::default(), AudioGains::new(100, 100))
            .expect("arm on real hardware");
        std::thread::sleep(LIVE);
        engine.stop().expect("clean stop");
        std::thread::sleep(SETTLE);
        samples.push(handles());
    }
    report("full arm", &samples);
}
