use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// Encoder rate-control strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// Peak-constrained VBR: hit the target on average, allow bursts up to the
    /// max cap on busy motion. The cap keeps the replay ring's RAM bounded.
    PeakVbr,
    /// Constant bitrate: the most predictable file size, at some quality cost.
    Cbr,
}

/// Engine configuration, loaded from `%APPDATA%\trix\config.toml`.
/// Missing file or missing keys fall back to defaults; a malformed file is
/// reported and ignored rather than aborting the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Target capture/encode frame rate.
    pub fps: u32,
    /// Target (average) video bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// VBR peak-bitrate cap in kbit/s. `0` = auto (1.5× the target). Ignored
    /// in CBR mode. This cap is also the replay ring's worst-case RAM bound.
    pub max_bitrate_kbps: u32,
    /// Rate control: `"vbr"` (peak-constrained, quality-leaning) or `"cbr"`
    /// (predictable size). Unknown values fall back to `"vbr"`.
    pub rate_control: String,
    /// Replay-buffer length in seconds (RAM cost scales with this × bitrate).
    pub replay_seconds: u32,
    /// Zero-based index of the monitor to capture.
    pub monitor_index: u32,
    /// Clip hotkey for `trix replay`: `mods+key`, e.g. "alt+f10" or
    /// "ctrl+shift+c". Modifiers: ctrl, alt, shift, win; keys: a-z, 0-9,
    /// f1-f24. Rebind when another overlay owns the default.
    pub clip_hotkey: String,
    /// GPU scheduling priority for Trix's own GPU work: `"low"` (below the
    /// game's — capture drops frames under contention instead of costing game
    /// fps) or `"normal"` (equal footing — smoother capture on a saturated
    /// GPU, at some game-fps cost). Unknown values fall back to `"low"`.
    pub gpu_priority: String,
    /// Seconds between performance self-reports (working set, CPU-vs-GPU
    /// memory split, per-frame latency). 0 (the default) disables the
    /// periodic line; a final summary is always logged at shutdown.
    pub stats_seconds: u32,
    /// Directory clips are written to. Empty (the default) resolves to
    /// `%USERPROFILE%\Videos\Trix`.
    pub clip_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fps: 60,
            bitrate_kbps: 8000,
            max_bitrate_kbps: 0,
            rate_control: "vbr".into(),
            replay_seconds: 15,
            monitor_index: 0,
            clip_hotkey: "alt+f10".into(),
            gpu_priority: "low".into(),
            stats_seconds: 0,
            clip_dir: String::new(),
        }
    }
}

impl Config {
    /// Parsed rate-control mode (defaults to VBR on an unknown value).
    pub fn rate_control(&self) -> RateControl {
        match self.rate_control.trim().to_ascii_lowercase().as_str() {
            "cbr" => RateControl::Cbr,
            "vbr" => RateControl::PeakVbr,
            other => {
                tracing::warn!(rate_control = other, "unknown rate_control, using vbr");
                RateControl::PeakVbr
            }
        }
    }

    /// True unless the user explicitly opted into `gpu_priority = "normal"`.
    pub fn gpu_priority_low(&self) -> bool {
        match self.gpu_priority.trim().to_ascii_lowercase().as_str() {
            "normal" => false,
            "low" => true,
            other => {
                tracing::warn!(gpu_priority = other, "unknown gpu_priority, using low");
                true
            }
        }
    }

    /// Target (average) video bitrate in bit/s.
    pub fn bitrate_bps(&self) -> u32 {
        self.bitrate_kbps.saturating_mul(1000)
    }

    /// VBR peak-cap in bit/s. `max_bitrate_kbps = 0` means auto (1.5× target);
    /// an explicit value is clamped to be at least the target.
    pub fn max_bitrate_bps(&self) -> u32 {
        let target = self.bitrate_bps();
        if self.max_bitrate_kbps == 0 {
            target.saturating_add(target / 2)
        } else {
            self.max_bitrate_kbps.saturating_mul(1000).max(target)
        }
    }

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

    /// Resolved clip directory (spec §5.1). A flat, timestamped folder —
    /// manual arming means there is no game name to fold on, and a flat
    /// directory sorts correctly in Explorer for people who never open the UI.
    pub fn clip_dir_path(&self) -> PathBuf {
        let configured = self.clip_dir.trim();
        if !configured.is_empty() {
            return PathBuf::from(configured);
        }
        std::env::var_os("USERPROFILE")
            .map(|home| PathBuf::from(home).join("Videos").join("Trix"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Writes the config back to `%APPDATA%\trix\config.toml`, creating the
    /// directory. Used by `config.set`.
    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("APPDATA is not set")?;
        self.save_to(&path)
    }

    /// [`Self::save`]'s body, against an explicit path.
    ///
    /// Exists so a test can persist somewhere other than the developer's real
    /// `%APPDATA%\trix\config.toml` — the same seam `pipe::serve_at` is to
    /// `pipe::serve`. Without it, exercising `config.set` at all would rewrite
    /// the settings of whoever ran `cargo test`.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_dir_defaults_under_the_user_profile() {
        let config = Config::default();
        let path = config.clip_dir_path();
        assert!(path.ends_with(r"Videos\Trix"), "unexpected default: {}", path.display());
    }

    #[test]
    fn an_explicit_clip_dir_wins() {
        let config = Config { clip_dir: r"D:\Clips".into(), ..Config::default() };
        assert_eq!(config.clip_dir_path(), PathBuf::from(r"D:\Clips"));
    }

    /// `deny_unknown_fields` means a config written by a newer build must not
    /// be silently ignored — and a config from before `clip_dir` existed must
    /// still load.
    #[test]
    fn a_config_without_clip_dir_still_loads() {
        let config: Config = toml::from_str("fps = 30\nreplay_seconds = 20\n").unwrap();
        assert_eq!(config.fps, 30);
        assert_eq!(config.replay_seconds, 20);
        assert_eq!(config.clip_dir, "");
    }

    #[test]
    fn config_round_trips_through_toml() {
        let original = Config { clip_dir: r"D:\Clips".into(), fps: 30, ..Config::default() };
        let text = toml::to_string_pretty(&original).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.fps, 30);
        assert_eq!(parsed.clip_dir, r"D:\Clips");
    }
}
