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
