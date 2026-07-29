//! `trix-daemon`: exposes the control protocol over `pipe::PIPE_NAME` so
//! something other than `trix.exe` can drive the engine. `status`, `arm`,
//! `disarm`, and `clip` are answered for real; the rest of the command set
//! lands in Tasks 6 and 7.
//!
//! This file is now only wiring: the state lives in `state::Daemon` and the
//! request handling in `dispatch`, both in this crate's library half so they
//! can be tested without a running process.

use std::sync::Arc;

use trix_core::config::Config;
use trix_core::control;
use trix_daemon::{pipe, state::Daemon};

/// Mirrors `trix-cli`'s `tracing_subscriber` setup (`crates/trix-cli/src/main.rs`):
/// `trix=info` by default, `trix=debug` under `-v`/`--verbose`, `RUST_LOG`
/// honoured when set. The daemon has no `clap` dependency, so the flag is
/// read straight off `std::env::args()` instead.
fn init_tracing() {
    let verbose = std::env::args().any(|arg| arg == "-v" || arg == "--verbose");
    let default_level = if verbose { "trix=debug" } else { "trix=info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .with_target(false)
        .compact()
        .init();
}

/// Polls `control::shutdown_requested()`, disarms, and exits the process once
/// it fires.
///
/// `pipe::serve` never returns and `control::install_shutdown_handler` only
/// sets a flag — it does not by itself unblock or kill anything. Without a
/// watcher the daemon would hang forever on Ctrl+C, blocked inside
/// `ConnectNamedPipe` waiting for a client that will never come. Interrupting
/// that blocking call cleanly would need overlapped IO — real unsafe
/// complexity — bought for a case (an idle daemon with no clients) that does
/// not need it.
///
/// The `disarm()` is what changed in this task, and it is not optional now that
/// the daemon can be armed: `std::process::exit` does not run destructors, so
/// without it a Ctrl+C landing while a clip is being muxed would kill the
/// process mid-write and cost the user exactly the clip they had just asked
/// for. `disarm` stops the engine and joins its thread, which finishes any
/// in-flight save. `mark_finalized()` is called after that, once, as the
/// process exits — never per engine session — releasing a blocked
/// console-close handler that would otherwise hold Windows open waiting for a
/// flush that has already happened.
fn spawn_shutdown_watcher(daemon: Arc<Daemon>) -> anyhow::Result<()> {
    use anyhow::Context as _;
    std::thread::Builder::new()
        .name("trix-shutdown-watcher".into())
        .spawn(move || {
            loop {
                if control::shutdown_requested() {
                    tracing::info!("shutdown requested, exiting");
                    if let Err(e) = daemon.disarm() {
                        tracing::warn!(error = %format!("{e:#}"), "disarm on shutdown failed");
                    }
                    control::mark_finalized();
                    std::process::exit(0);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })
        .context("failed to spawn the shutdown watcher thread")?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = Config::load();
    tracing::debug!(?config, "loaded configuration");

    control::install_shutdown_handler()?;

    // Deliberately not `control::acquire_single_instance()` here: that mutex
    // is the capture-session slot, taken on `arm` and released on `disarm`
    // (see `state.rs`). Acquiring it at daemon startup would make the daemon
    // and `trix.exe` mutually exclusive for the daemon's whole lifetime. The
    // daemon's own single-instance guarantee is `FILE_FLAG_FIRST_PIPE_INSTANCE`
    // on the pipe name (see `pipe.rs`).
    let daemon = Arc::new(Daemon::new(config));
    spawn_shutdown_watcher(Arc::clone(&daemon))?;

    // `pipe::serve` logs "listening on {PIPE_NAME}" itself, once the first
    // pipe instance is actually bound — see the comment in `pipe.rs`.
    pipe::serve(daemon)
}
