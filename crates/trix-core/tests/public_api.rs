//! Proves the engine is reachable from outside the crate. If the daemon
//! (plan 3) can't call these, neither can this test.

use std::path::PathBuf;

use trix_core::config::{Config, RateControl};
use trix_core::record::RecordOptions;
use trix_core::replay::ReplayOptions;

#[test]
fn config_is_publicly_constructible() {
    let config = Config::default();
    assert_eq!(config.fps, 60);
    assert_eq!(config.bitrate_kbps, 8000);
    assert_eq!(config.stats_seconds, 0, "stats must stay off by default");
    assert_eq!(config.rate_control(), RateControl::PeakVbr);
    assert!(config.gpu_priority_low(), "gpu priority must default to low");
}

#[test]
fn session_option_structs_are_publicly_constructible() {
    let _record = RecordOptions {
        duration_secs: 10,
        output: PathBuf::from("out.mp4"),
        no_audio: false,
    };
    let _replay = ReplayOptions { auto_clip_secs: Some(8), exit_after_secs: Some(12) };
}

#[test]
fn bitrate_helpers_are_public() {
    let config = Config::default();
    assert_eq!(config.bitrate_bps(), 8_000_000);
    assert_eq!(config.max_bitrate_bps(), 12_000_000, "0 means auto = 1.5x target");
}

/// Binds every engine entry point the daemon (plan 3) will call as a function
/// pointer. This type-checks each signature at compile time and fails the
/// build the moment a module the daemon needs (e.g. `stats`, `encode`,
/// `capture`) goes private or a signature drifts — without ever executing
/// anything. `record::run`, `replay::run`, and `probe::run` capture the
/// screen, `control::acquire_single_instance` registers a process-wide mutex,
/// and `capture::video::snapshot` starts a capture session, so none of these
/// may actually be called from a test.
#[test]
fn engine_entry_points_are_public() {
    let _: fn(&Config, RecordOptions) -> anyhow::Result<()> = trix_core::record::run;
    let _: fn(&Config, ReplayOptions) -> anyhow::Result<()> = trix_core::replay::run;
    let _: fn() -> anyhow::Result<()> = trix_core::probe::run;
    let _: fn() -> anyhow::Result<()> = trix_core::control::acquire_single_instance;

    // Plain CPU-side data structure — safe to actually construct.
    let _ = trix_core::stats::LatencyHistogram::new();

    // `encode`: bound, not called — MFStartup has a real (if idempotent)
    // process-wide side effect and this test must stay a pure type-check.
    let _: fn() -> anyhow::Result<()> = trix_core::encode::mf::ensure_mf_started;

    // `capture`: bound, not called — calling this would start a real WGC
    // capture session.
    let _: fn(u32, std::path::PathBuf) -> anyhow::Result<()> = trix_core::capture::video::snapshot;
}
