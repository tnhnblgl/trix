use trix_core::{capture, config, control, probe, record, replay};

use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use clap::{Parser, Subcommand};

/// Phase 0 spike for the Desktop Duplication backend. Temporary by design —
/// see the module doc for what deleting it looks like.
mod dd_probe;

#[derive(Parser)]
#[command(
    name = "trix",
    version,
    about = "Trix core engine — ultra-lightweight screen/clip recorder"
)]
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
    /// SPIKE: open a DXGI Desktop Duplication and report what it delivers
    ///
    /// Throwaway diagnostic. It records nothing and writes no files — it opens
    /// a duplication of one monitor, counts frames for a few seconds, and
    /// prints. The question it exists to answer is whether Windows draws its
    /// yellow capture border while this runs.
    DdProbe {
        /// Which monitor, in `trix probe` numbering. Defaults to the one the
        /// config already captures, so the probe targets the same screen Trix
        /// would.
        #[arg(long, value_name = "INDEX")]
        monitor: Option<u32>,
        /// How long to hold the duplication open
        #[arg(long, default_value_t = 20, value_name = "SECONDS")]
        seconds: u64,
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
    /// Wait for a process to exit, then start trix-ui.exe from beside this exe
    ///
    /// Used by the updater. The app cannot relaunch itself directly: the new
    /// process would start while the old one is still alive, and
    /// tauri-plugin-single-instance would hand it to the dying instance and
    /// exit, leaving nothing running.
    #[command(hide = true)]
    RestartUi {
        /// The process to wait for -- the trix-ui.exe that is updating
        #[arg(long, value_name = "PID")]
        wait_pid: u32,
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
        // Deliberately outside the single-instance lock: the point of the
        // spike is to run it *while* Trix is armed, and taking the lock would
        // make the one interesting case impossible.
        Command::DdProbe { monitor, seconds } => {
            dd_probe::run(monitor.unwrap_or(config.monitor_index), seconds)
        }
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
        Command::RestartUi { wait_pid } => restart_ui(wait_pid),
    }
}

/// How long to wait for the old `trix-ui.exe` to exit before giving up and
/// starting the new one anyway.
///
/// Not `INFINITE`: the wait is keyed on a process *ID*, and Windows recycles
/// those. Between the old `trix-ui.exe` reading its own PID and this code
/// calling `OpenProcess` on it, that number can already belong to an
/// unrelated, long-lived process -- and `INFINITE` on the wrong process could
/// mean the update never comes back, silently, with this process orphaned
/// until reboot. A bounded wait is strictly better on both real paths: the
/// normal case is already dead within milliseconds, and in the recycled-PID
/// case the real `trix-ui.exe` is long gone by the time the cap expires, so
/// starting anyway does not reopen the single-instance race this subcommand
/// exists to prevent.
const WAIT_FOR_OLD_UI_MS: u32 = 60_000;
// Generous next to the few-millisecond normal case, but finite -- well short
// of the `u32::MAX` Windows treats as INFINITE -- so a recycled PID cannot
// hang this forever. Compile-time rather than a #[test]: both sides are known
// at compile time, so a runtime assertion on a constant would only trip
// clippy's assertions_on_constants lint, and a bad value should fail the
// build, not a test run.
const _: () = assert!(WAIT_FOR_OLD_UI_MS >= 30_000 && WAIT_FOR_OLD_UI_MS < u32::MAX);

/// How long to pause before starting the new UI when `OpenProcess` itself
/// fails.
///
/// A failure is not proof `pid` is gone: `ERROR_ACCESS_DENIED` against a
/// process that is very much alive looks the same from here as "no such
/// process". Spawning instantly on any failure would recreate the exact
/// single-instance race this subcommand exists to prevent, so this pauses
/// instead of assuming -- just long enough to outlast the moment between the
/// old UI spawning this process and the old UI actually exiting.
const GRACE_ON_OPEN_FAILURE_MS: u64 = 2_000;
// Much shorter than WAIT_FOR_OLD_UI_MS: this is a brief grace period, not a
// second wait loop. See WAIT_FOR_OLD_UI_MS's comment for why this is a
// compile-time assertion rather than a #[test].
const _: () = assert!(GRACE_ON_OPEN_FAILURE_MS * 10 <= WAIT_FOR_OLD_UI_MS as u64);

/// Waits for `pid` to exit, then starts `trix-ui.exe` from beside this binary.
///
/// The wait is on a real process handle rather than a poll, so there is no
/// window in which the new app starts while the old one still holds the
/// single-instance lock -- but only up to [`WAIT_FOR_OLD_UI_MS`], and a
/// failed `OpenProcess` gets a short pause rather than an instant launch. See
/// those constants' doc comments for why: a bare `INFINITE` wait or a "failed
/// means gone" assumption both trust a process ID that Windows is free to
/// have already handed to someone else.
fn restart_ui(pid: u32) -> Result<()> {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(handle) => {
                let outcome = WaitForSingleObject(handle, WAIT_FOR_OLD_UI_MS);
                let _ = CloseHandle(handle);
                if outcome != WAIT_OBJECT_0 {
                    tracing::warn!(
                        pid,
                        outcome = outcome.0,
                        "did not see the old trix-ui.exe exit before the timeout; starting the new one anyway"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    pid,
                    %error,
                    "could not open the old trix-ui.exe process; pausing before starting the new one anyway"
                );
                std::thread::sleep(std::time::Duration::from_millis(GRACE_ON_OPEN_FAILURE_MS));
            }
        }
    }

    let exe = std::env::current_exe().context("could not locate trix.exe")?;
    let ui = exe
        .parent()
        .map(|dir| dir.join("trix-ui.exe"))
        .context("could not work out where trix-ui.exe is")?;
    std::process::Command::new(&ui)
        .spawn()
        .with_context(|| format!("could not start {}", ui.display()))?;
    Ok(())
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
        assert!(Cli::try_parse_from(["trix", "dd-probe"]).is_ok());
        assert!(
            Cli::try_parse_from(["trix", "dd-probe", "--monitor", "1", "--seconds", "30"]).is_ok()
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
        let cli = Cli::try_parse_from(["trix", "replay", "--auto-clip", "8", "--exit-after", "12"])
            .unwrap();
        let Command::Replay { auto_clip, exit_after } = cli.command else {
            panic!("expected Replay");
        };
        assert_eq!(auto_clip, Some(8));
        assert_eq!(exit_after, Some(12));
    }

    /// A bare `dd-probe` must fall through to the configured monitor rather
    /// than hard-coding screen 0 — the border is a property of the screen Trix
    /// actually captures, so probing a different one would answer the wrong
    /// question on a multi-monitor machine.
    ///
    /// Delete this with the subcommand when the spike is retired.
    #[test]
    fn dd_probe_defaults_to_the_configured_monitor_and_twenty_seconds() {
        let cli = Cli::try_parse_from(["trix", "dd-probe"]).unwrap();
        let Command::DdProbe { monitor, seconds } = cli.command else {
            panic!("expected DdProbe");
        };
        assert_eq!(monitor, None, "no --monitor means the config's monitor_index, not 0");
        assert_eq!(seconds, 20);

        let cli = Cli::try_parse_from(["trix", "dd-probe", "--monitor", "2"]).unwrap();
        let Command::DdProbe { monitor, .. } = cli.command else {
            panic!("expected DdProbe");
        };
        assert_eq!(monitor, Some(2), "an explicit --monitor must still win");
    }

    /// Guards `crates/trix-cli/Cargo.toml`'s `[[bin]] name = "trix"` — nothing
    /// else in the test suite would notice if it were renamed, but scripts
    /// and muscle memory depend on `trix.exe` staying `trix.exe`.
    #[test]
    fn binary_name_is_trix() {
        assert_eq!(env!("CARGO_BIN_NAME"), "trix");
    }

    /// Pins the subcommand name, the `--wait-pid` flag name, and that it stays
    /// hidden from `--help`. The updater spawns this by name, and a rename or a
    /// changed flag would break that relaunch silently, leaving the user with
    /// an updated install and no running app.
    ///
    /// The other half of the pair is `restart_args` in
    /// `crates/trix-ui/src/update/mod.rs`, pinned there by
    /// `the_relaunch_sends_the_command_line_trix_exe_parses`. Nothing links the
    /// two crates -- `trix-ui` must not depend on `trix-core`, and so cannot
    /// depend on this one -- so the names have to be changed in both places or
    /// in neither, and both suites stay green either way. Changing this test is
    /// the moment to go and change that one.
    #[test]
    fn restart_ui_parses_the_flag_the_updater_sends() {
        let cli =
            Cli::try_parse_from(["trix", "restart-ui", "--wait-pid", "4321"]).expect("parses");
        assert!(matches!(cli.command, Command::RestartUi { wait_pid: 4321 }));
    }

    /// Hidden from --help: it is machinery, not a feature, and a user who runs
    /// it by hand gets a process that waits for a PID that is not there.
    #[test]
    fn restart_ui_is_hidden_from_help() {
        use clap::CommandFactory as _;

        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("restart-ui"), "restart-ui must not appear in --help");
    }
}
