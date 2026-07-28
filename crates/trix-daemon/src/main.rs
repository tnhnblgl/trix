//! `trix-daemon`: exposes the control protocol over `pipe::PIPE_NAME` so
//! something other than `trix.exe` can drive the engine. This task wires up
//! the transport only — the daemon answers exactly one real command
//! (`status`); Task 5 replaces the rest.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use trix_core::config::Config;
use trix_core::control;
use trix_daemon::{clients, pipe};
use trix_proto::{Command, Request, Response};

/// Answers `status` from the loaded config; every other command is either a
/// `Command::parse` error or the placeholder below — Task 5 wires up the
/// engine behind these.
///
/// Still tracks real per-connection identity through `clients::Clients` —
/// multi-client bookkeeping is transport, not engine state, so it belongs in
/// this task. Nothing broadcasts to the registry yet (there are no events to
/// send), which is why `Clients::broadcast`/`broadcast_stats`/`set_stats` have
/// no caller here and stay exercised only by `clients.rs`'s own tests until
/// Task 5 gives them one.
struct StatusOnlyHandler {
    config: Config,
    clients: clients::Clients,
}

impl pipe::ClientHandler for StatusOnlyHandler {
    fn client_connected(&self, out: Sender<String>) -> u64 {
        self.clients.register(out)
    }

    fn client_disconnected(&self, client: u64) {
        self.clients.unregister(client);
    }

    fn dispatch(&self, _client: u64, request: &Request) -> Response {
        match Command::parse(request) {
            Ok(Command::Status) => Response::ok(request.id, status_json(&self.config)),
            Ok(_) => Response::err(request.id, "not available until the engine is wired"),
            Err(error) => Response::err(request.id, error),
        }
    }
}

/// The `status` payload's shape is spec-fixed (§4.2's example) — every field
/// present even though most are placeholders until Task 5 wires up the
/// engine: `armed`, `ring_seconds_used`, and `encoder` all describe engine
/// state this task deliberately does not have.
///
/// Built with explicit `Value::from` conversions rather than `serde_json::json!`:
/// every arm of that macro other than its `true`/`false`/`null`/array/object
/// literals expands to `serde_json::to_value(&x).unwrap()` — an `unwrap` this
/// function's caller (`dispatch`) reaches straight from a socket message. The
/// `From` impls used here (integers, `f64`, `Cow<str>`, `Option`) cannot fail,
/// so there is nothing to unwrap.
fn status_json(config: &Config) -> serde_json::Value {
    use serde_json::{Map, Value};
    let mut fields = Map::new();
    fields.insert("armed".to_string(), Value::Bool(false));
    fields.insert("encoder".to_string(), Value::Null);
    fields.insert("monitor_index".to_string(), Value::from(config.monitor_index));
    fields.insert("ring_seconds_used".to_string(), Value::from(0.0_f64));
    fields.insert("ring_seconds_total".to_string(), Value::from(config.replay_seconds));
    fields.insert("version".to_string(), Value::from(env!("CARGO_PKG_VERSION")));
    fields.insert("clip_dir".to_string(), Value::from(config.clip_dir_path().to_string_lossy()));
    Value::Object(fields)
}

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

/// Polls `control::shutdown_requested()` and exits the process once it fires.
///
/// The brief says the daemon "installs the shutdown handler", but
/// `pipe::serve` never returns and `control::install_shutdown_handler` only
/// sets a flag — it does not by itself unblock or kill anything. Without a
/// watcher the daemon would hang forever on Ctrl+C, blocked inside
/// `ConnectNamedPipe` waiting for a client that will never come. Interrupting
/// that blocking call cleanly would need overlapped IO — real unsafe
/// complexity — bought for a case (an idle daemon with no clients) that does
/// not need it. This task has no engine state to flush, so exiting
/// immediately is correct; Task 5 will do real work before this exit once
/// there is a ring buffer that needs to finish a write. `mark_finalized()` is
/// called here, once, as the daemon process exits — never per engine
/// session — releasing a blocked console-close handler that would otherwise
/// hold Windows open for an in-flight MP4 flush that, in this task, never
/// happens.
fn spawn_shutdown_watcher() -> anyhow::Result<()> {
    use anyhow::Context as _;
    std::thread::Builder::new()
        .name("trix-shutdown-watcher".into())
        .spawn(|| {
            loop {
                if control::shutdown_requested() {
                    tracing::info!("shutdown requested, exiting");
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
    spawn_shutdown_watcher()?;

    // Deliberately not `control::acquire_single_instance()` here: that mutex
    // is the capture-session slot, taken on `arm` and released on `disarm`
    // (Task 5). Acquiring it at daemon startup would make the daemon and
    // `trix.exe` mutually exclusive for the daemon's whole lifetime. The
    // daemon's own single-instance guarantee for this task is
    // `FILE_FLAG_FIRST_PIPE_INSTANCE` on the pipe name (see `pipe.rs`).
    let handler = Arc::new(StatusOnlyHandler { config, clients: clients::Clients::default() });

    // `pipe::serve` logs "listening on {PIPE_NAME}" itself, once the first
    // pipe instance is actually bound — see the comment in `pipe.rs`.
    pipe::serve(handler)
}
