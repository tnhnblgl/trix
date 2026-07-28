use trix_core::{capture, config, control, probe, record, replay};

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "trix", version, about = "Trix core engine — ultra-lightweight screen/clip recorder")]
struct Cli {
    /// Enable verbose (debug-level) logging
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Enumerate OS version, monitors, and hardware video encoders
    Probe {
        /// Capture one frame, save it as PNG, and exit
        #[arg(long, num_args = 0..=1, default_missing_value = "snapshot.png", value_name = "PATH")]
        snapshot: Option<PathBuf>,
        /// Measure capture frame-arrival cadence for N seconds
        #[arg(long, value_name = "SECONDS")]
        capture: Option<u64>,
        /// Capture N seconds of system loopback audio to a WAV file
        #[arg(long, value_name = "SECONDS")]
        audio: Option<u64>,
    },
    /// Record the screen to an MP4 file
    Record {
        /// Recording length in seconds
        #[arg(short, long, default_value_t = 10)]
        duration: u64,
        /// Output file path
        #[arg(short, long, default_value = "output.mp4")]
        output: PathBuf,
        /// Disable system-audio capture
        #[arg(long)]
        no_audio: bool,
    },
    /// Run the replay buffer; the clip hotkey (default Alt+F10) saves the last N seconds
    Replay {
        /// Testing: save a clip automatically after N seconds
        #[arg(long, value_name = "SECONDS", hide = true)]
        auto_clip: Option<u64>,
        /// Testing: exit after N seconds instead of running until Ctrl+C
        #[arg(long, value_name = "SECONDS", hide = true)]
        exit_after: Option<u64>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let default_level = if cli.verbose { "trix=debug" } else { "trix=info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .with_target(false)
        .compact()
        .init();

    let config = config::Config::load();
    tracing::debug!(?config, "loaded configuration");

    control::install_shutdown_handler()?;

    match cli.command {
        Command::Probe { snapshot, capture, audio } => match (snapshot, capture, audio) {
            (Some(path), _, _) => capture::video::snapshot(config.monitor_index, path),
            (None, Some(seconds), _) => capture::video::measure(config.monitor_index, seconds),
            (None, None, Some(seconds)) => {
                capture::audio::record_wav(seconds, std::path::Path::new("audio_probe.wav"))
            }
            (None, None, None) => probe::run(),
        },
        Command::Record { duration, output, no_audio } => {
            let _single = control::acquire_single_instance()?;
            record::run(
                &config,
                record::RecordOptions { duration_secs: duration, output, no_audio },
            )
        }
        Command::Replay { auto_clip, exit_after } => {
            let _single = control::acquire_single_instance()?;
            replay::run(
                &config,
                replay::ReplayOptions {
                    auto_clip_secs: auto_clip,
                    exit_after_secs: exit_after,
                    print_clips: true,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every subcommand and flag the verification workflow depends on.
    /// If the split drops one, this fails instead of a test script failing
    /// three phases later.
    #[test]
    fn cli_surface_is_unchanged() {
        assert!(Cli::try_parse_from(["trix", "probe"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "probe", "--snapshot"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "probe", "--snapshot", "s.png"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "probe", "--capture", "5"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "probe", "--audio", "5"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "record"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "record", "-d", "10", "-o", "o.mp4"]).is_ok());
        assert!(
            Cli::try_parse_from(["trix", "record", "--duration", "10", "--output", "o.mp4"])
                .is_ok(),
            "long forms must keep working, not just the short flags"
        );
        assert!(Cli::try_parse_from(["trix", "record", "--no-audio"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "replay"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "-v", "replay"]).is_ok());
        assert!(
            Cli::try_parse_from(["trix", "--verbose", "replay"]).is_ok(),
            "long form of -v must keep working"
        );
        assert!(
            Cli::try_parse_from(["trix", "replay", "--auto-clip", "8", "--exit-after", "12"])
                .is_ok(),
            "the hidden test flags are how every phase gets verified"
        );
        assert!(Cli::try_parse_from(["trix", "bogus"]).is_err());
        assert!(
            Cli::try_parse_from(["trix"]).is_err(),
            "a subcommand must be required — trix with no arguments should not parse"
        );
    }

    #[test]
    fn record_defaults_match_the_pre_split_binary() {
        let cli = Cli::try_parse_from(["trix", "record"]).unwrap();
        let Command::Record { duration, output, no_audio } = cli.command else {
            panic!("expected Record");
        };
        assert_eq!(duration, 10);
        assert_eq!(output, std::path::PathBuf::from("output.mp4"));
        assert!(!no_audio);
    }

    /// `--snapshot` with no argument must still resolve to a path — deleting
    /// `default_missing_value` from the `probe` command's `snapshot` arg
    /// would silently turn this into `None` and this must catch it.
    #[test]
    fn snapshot_flag_defaults_to_snapshot_png_when_bare() {
        let cli = Cli::try_parse_from(["trix", "probe", "--snapshot"]).unwrap();
        let Command::Probe { snapshot, .. } = cli.command else {
            panic!("expected Probe");
        };
        assert_eq!(snapshot, Some(PathBuf::from("snapshot.png")));
    }

    /// The hidden `--auto-clip` / `--exit-after` flags are how every replay
    /// phase gets verified end-to-end; asserting `is_ok()` alone would not
    /// notice the parsed values silently going wrong.
    #[test]
    fn replay_test_hooks_parse_to_expected_values() {
        let cli =
            Cli::try_parse_from(["trix", "replay", "--auto-clip", "8", "--exit-after", "12"])
                .unwrap();
        let Command::Replay { auto_clip, exit_after } = cli.command else {
            panic!("expected Replay");
        };
        assert_eq!(auto_clip, Some(8));
        assert_eq!(exit_after, Some(12));
    }

    /// Guards `crates/trix-cli/Cargo.toml`'s `[[bin]] name = "trix"` — nothing
    /// else in the test suite would notice if it were renamed, but scripts
    /// and muscle memory depend on `trix.exe` staying `trix.exe`.
    #[test]
    fn binary_name_is_trix() {
        assert_eq!(env!("CARGO_BIN_NAME"), "trix");
    }
}
