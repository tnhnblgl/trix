//! `trix-daemon`: exposes the control protocol over `pipe::PIPE_NAME` so
//! something other than `trix.exe` can drive the engine. The whole command set
//! is answered for real — `status`, `arm`, `disarm`, `clip`, the clip library
//! (`library.list`, `delete`, `rename`, `favorite`, `reveal`), the settings
//! (`config.get`, `config.set`), the hardware enumerations (`monitors.list`,
//! `encoders.list`), and `stats.subscribe`.
//!
//! This file is now only wiring: the state lives in `state::Daemon`, the
//! request handling in `dispatch`, and the stats thread's timing loop in
//! `stats`, all in this crate's library half so they can be tested without a
//! running process. What is left here is what genuinely cannot be: reading the
//! command line, installing the console-control handler, and the shutdown
//! watcher that calls `std::process::exit`.

use std::sync::{Arc, OnceLock};

use trix_core::config::Config;
use trix_core::control;
use trix_daemon::stats::{STATS_POLL, spawn_stats_thread};
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

/// How long shutdown waits for `disarm` before exiting anyway.
///
/// A budget rather than an unbounded wait, because `disarm` takes the `armed`
/// lock and that lock is held across `EngineHandle::spawn` (an untimed
/// readiness handshake) and `EngineHandle::clip` (a channel wait that survives
/// display rebuilds). If either wedges, an unbounded `disarm` here would make
/// Ctrl+C do *nothing at all*: `on_console_ctrl` returns `TRUE` for
/// `CTRL_C_EVENT` with no OS deadline behind it, so this `process::exit` is the
/// only thing that ends the process, and `taskkill` would be the user's only
/// way out. Three seconds is long enough for an honest finalize — the mux of a
/// ring already in memory — and short enough that a user who pressed Ctrl+C
/// does not conclude the daemon is hung.
///
/// It must also stay *under* the console handler's own wait, or the disarm it
/// exists to protect is pointless on the paths that need it most. For a console
/// close, logoff, or OS shutdown, `on_console_ctrl`
/// (`trix-core/src/control.rs`) parks its thread in `for _ in 0..80` at 50 ms —
/// exactly 4000 ms — waiting for `mark_finalized()`, and Windows kills the
/// process the moment that handler returns. A 4 s budget plus this watcher's
/// 100 ms poll would signal at ~4100 ms: the handler has already given up and
/// the mux dies mid-write, which is the exact loss the disarm was added to
/// prevent. Three seconds leaves roughly 900 ms of margin.
const SHUTDOWN_DISARM_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// Polls `control::shutdown_requested()`, disarms within a budget, and exits
/// the process once it fires.
///
/// `pipe::serve` never returns and `control::install_shutdown_handler` only
/// sets a flag — it does not by itself unblock or kill anything. Without a
/// watcher the daemon would hang forever on Ctrl+C, blocked inside
/// `ConnectNamedPipe` waiting for a client that will never come. Interrupting
/// that blocking call cleanly would need overlapped IO — real unsafe
/// complexity — bought for a case (an idle daemon with no clients) that does
/// not need it.
///
/// The `disarm()` is not optional now that the daemon can be armed:
/// `std::process::exit` does not run destructors, so without it a Ctrl+C
/// landing while a clip is being muxed would kill the process mid-write and
/// cost the user exactly the clip they had just asked for. `disarm` stops the
/// engine and joins its thread, which finishes any in-flight save. It runs on a
/// helper thread under [`SHUTDOWN_DISARM_BUDGET`] so that a wedged engine
/// delays the exit instead of preventing it.
///
/// `mark_finalized()` is called after that — once, on both the finished and the
/// timed-out path, immediately before the exit — releasing a blocked
/// console-close handler that would otherwise hold Windows open waiting for a
/// flush that has already happened, or that is never going to happen.
///
/// The daemon arrives through a `OnceLock` rather than as a value because this
/// watcher is installed *before* `Daemon::new`: that constructor scans the clip
/// library off the disk, and a slow or unresponsive clip directory must not be
/// a window in which Ctrl+C does nothing. Until the slot is filled there is
/// nothing armed to disarm, so shutting down during the scan simply exits.
fn spawn_shutdown_watcher(daemon: Arc<OnceLock<Arc<Daemon>>>) -> anyhow::Result<()> {
    use anyhow::Context as _;
    std::thread::Builder::new()
        .name("trix-shutdown-watcher".into())
        .spawn(move || {
            loop {
                if control::shutdown_requested() {
                    tracing::info!("shutdown requested, exiting");
                    if let Some(daemon) = daemon.get() {
                        disarm_within_budget(Arc::clone(daemon));
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

/// Runs `daemon.disarm()` on a helper thread and waits at most
/// [`SHUTDOWN_DISARM_BUDGET`] for it. Returns either way — the caller exits the
/// process next, and an exit that loses a clip is still better than one that
/// never happens.
fn disarm_within_budget(daemon: Arc<Daemon>) {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let spawned =
        std::thread::Builder::new().name("trix-shutdown-disarm".into()).spawn(move || {
            if let Err(e) = daemon.disarm() {
                tracing::warn!(error = %format!("{e:#}"), "disarm on shutdown failed");
            }
            let _ = done_tx.send(());
        });
    match spawned {
        // The thread is deliberately not joined: on the timeout path it is
        // still inside `disarm`, and joining it is the unbounded wait this
        // whole function exists to avoid. `process::exit` takes it with us.
        Ok(_) => {
            if done_rx.recv_timeout(SHUTDOWN_DISARM_BUDGET).is_err() {
                tracing::warn!(
                    seconds = SHUTDOWN_DISARM_BUDGET.as_secs(),
                    "disarm did not finish in time; exiting anyway"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not spawn the disarm thread; exiting anyway"),
    }
}

fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = Config::load();
    tracing::debug!(?config, "loaded configuration");

    control::install_shutdown_handler()?;

    // Installed before `Daemon::new`, which scans the clip library off the
    // disk: the handler above only sets a flag, so until this watcher is
    // running nothing acts on it, and a slow clip directory (a network path, a
    // spun-down drive) would be startup time in which Ctrl+C does nothing at
    // all. The daemon is handed over through the slot once it exists.
    let slot = Arc::new(OnceLock::new());
    spawn_shutdown_watcher(Arc::clone(&slot))?;

    // Deliberately not `control::acquire_single_instance()` here: that mutex
    // is the capture-session slot, taken on `arm` and released on `disarm`
    // (see `state.rs`). Acquiring it at daemon startup would make the daemon
    // and `trix.exe` mutually exclusive for the daemon's whole lifetime. The
    // daemon's own single-instance guarantee is `FILE_FLAG_FIRST_PIPE_INSTANCE`
    // on the pipe name (see `pipe.rs`).
    let daemon = Arc::new(Daemon::new(config));
    // `set` can only fail if something already filled the slot, and nothing
    // else ever writes to it.
    let _ = slot.set(Arc::clone(&daemon));

    // Started before the socket is listening so the first client to subscribe
    // is already being served by a running thread. It idles at one wakeup every
    // `STATS_POLL` until somebody actually subscribes. The loop itself lives in
    // the library half (`stats.rs`) so it can be tested at a millisecond poll;
    // the production interval is passed here and nowhere else.
    spawn_stats_thread(Arc::clone(&daemon), STATS_POLL)?;

    // `pipe::serve` logs "listening on {PIPE_NAME}" itself, once the first
    // pipe instance is actually bound — see the comment in `pipe.rs`.
    pipe::serve(daemon)
}
