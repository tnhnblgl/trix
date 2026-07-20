use std::path::PathBuf;

use serde::Deserialize;

/// Engine configuration, loaded from `%APPDATA%\trix\config.toml`.
/// Missing file or missing keys fall back to defaults; a malformed file is
/// reported and ignored rather than aborting the daemon.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Target capture/encode frame rate.
    pub fps: u32,
    /// Video bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// Replay-buffer length in seconds (RAM cost scales with this × bitrate).
    pub replay_seconds: u32,
    /// Zero-based index of the monitor to capture.
    pub monitor_index: u32,
    /// Clip hotkey for `trix replay`: `mods+key`, e.g. "alt+f10" or
    /// "ctrl+shift+c". Modifiers: ctrl, alt, shift, win; keys: a-z, 0-9,
    /// f1-f24. Rebind when another overlay owns the default.
    pub clip_hotkey: String,
    /// Seconds between performance self-reports (working set, CPU-vs-GPU
    /// memory split, per-frame latency). 0 disables the periodic line;
    /// a final summary is always logged at shutdown.
    pub stats_seconds: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fps: 60,
            bitrate_kbps: 8000,
            replay_seconds: 15,
            monitor_index: 0,
            clip_hotkey: "alt+f10".into(),
            stats_seconds: 60,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        let appdata = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(appdata).join("trix").join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "ignoring malformed config");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
}
