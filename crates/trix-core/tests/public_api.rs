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
    let _record =
        RecordOptions { duration_secs: 10, output: PathBuf::from("out.mp4"), no_audio: false };
    let _replay =
        ReplayOptions { auto_clip_secs: Some(8), exit_after_secs: Some(12), print_clips: true };
}

#[test]
fn bitrate_helpers_are_public() {
    let config = Config::default();
    assert_eq!(config.bitrate_bps(), 8_000_000);
    assert_eq!(config.max_bitrate_bps(), 12_000_000, "0 means auto = 1.5x target");
}

/// Guards the clip-library module the daemon (plan 3) reads and writes clips
/// through. `allocate_clip_id`, `write_sidecar`, `read_sidecar`, and `scan`
/// touch the filesystem, so — like `capture`/`encode` above — they're bound
/// as function pointers rather than called; the rest are pure and cheap
/// enough to call for real, like `stats` above.
#[test]
fn library_module_is_public() {
    use std::path::{Path, PathBuf};

    let _: fn(&Path) -> anyhow::Result<String> = trix_core::library::allocate_clip_id;
    let _: fn(&Path, &trix_proto::ClipMeta) -> anyhow::Result<()> =
        trix_core::library::write_sidecar;
    let _: fn(&Path) -> anyhow::Result<trix_proto::ClipMeta> = trix_core::library::read_sidecar;
    let _: fn(&Path) -> anyhow::Result<Vec<trix_proto::ClipMeta>> = trix_core::library::scan;

    let dir = PathBuf::from("clips");
    assert_eq!(trix_core::library::mp4_path(&dir, "id"), dir.join("id.mp4"));
    assert_eq!(trix_core::library::sidecar_path(&dir, "id"), dir.join("id.json"));
    assert_eq!(trix_core::library::thumb_path(&dir, "id"), dir.join("id.jpg"));
    assert!(trix_core::library::is_valid_id("20260726_143012"));
    assert!(!trix_core::library::is_valid_id(".."));
    assert_eq!(
        trix_core::library::format_rfc3339(2026, 1, 1, 0, 0, 0, 0),
        "2026-01-01T00:00:00+00:00"
    );
    let _: fn() -> String = trix_core::library::now_rfc3339_local;
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
    let _: fn() -> anyhow::Result<trix_core::control::SingleInstance> =
        trix_core::control::acquire_single_instance;

    // Plain CPU-side data structure — safe to actually construct.
    let _ = trix_core::stats::LatencyHistogram::new();

    // The daemon's `monitors.list` / `encoders.list` and the device-free half
    // of the memory report, which its `stats` event calls with no capture
    // session anywhere. Bound rather than called for the same reason as the
    // rest of this test: `encoders` runs `MFStartup`.
    let _: fn() -> anyhow::Result<Vec<trix_core::probe::MonitorInfo>> = trix_core::probe::monitors;
    let _: fn() -> anyhow::Result<Vec<trix_core::probe::EncoderInfo>> = trix_core::probe::encoders;
    let _: fn() -> u64 = trix_core::stats::working_set;
    let _: fn(&Config, &std::path::Path) -> anyhow::Result<()> = Config::save_to;

    // `encode`: bound, not called — MFStartup has a real (if idempotent)
    // process-wide side effect and this test must stay a pure type-check.
    let _: fn() -> anyhow::Result<()> = trix_core::encode::mf::ensure_mf_started;

    // `capture`: bound, not called — calling this would start a real WGC
    // capture session.
    let _: fn(u32, std::path::PathBuf) -> anyhow::Result<()> = trix_core::capture::video::snapshot;
}

/// Guards the command-driven engine the daemon (plan 3) arms, polls, clips
/// from, and disarms. Every entry point is bound rather than called: `spawn`
/// and `run_driven` start a real capture session, and the rest need one.
#[test]
#[allow(clippy::type_complexity)] // a bound signature is the point of this test
fn engine_handle_is_public() {
    use std::sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender},
    };

    use trix_core::engine::{EngineCommand, EngineHandle, EngineStatus};

    let _: fn(
        Config,
        std::sync::Arc<trix_core::capture::audio::AudioGains>,
    ) -> anyhow::Result<EngineHandle> = EngineHandle::spawn;
    let _: fn(&EngineHandle) -> EngineStatus = EngineHandle::status;
    let _: fn(&EngineHandle) -> anyhow::Result<Option<trix_proto::ClipMeta>> = EngineHandle::clip;
    let _: fn(EngineHandle) -> anyhow::Result<()> = EngineHandle::stop;
    // The readiness channel carries the first session's `EngineStatus`, not a
    // unit: `EngineHandle::spawn` publishes it before returning so an `arm`
    // response can name its encoder without a follow-up poll.
    let _: fn(
        &Config,
        Arc<trix_core::capture::audio::AudioGains>,
        Receiver<EngineCommand>,
        Arc<Mutex<EngineStatus>>,
        Option<Sender<anyhow::Result<EngineStatus>>>,
    ) -> anyhow::Result<()> = trix_core::replay::run_driven;

    // Plain CPU-side data — safe to actually construct.
    let status = EngineStatus::default();
    assert_eq!(status.ring_seconds_used, 0.0);
    let _stop = EngineCommand::Stop;
}
