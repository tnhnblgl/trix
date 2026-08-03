//! Everything the webview is allowed to ask the Rust half to do.
//!
//! Three commands, and two of them are about the daemon being absent. There is
//! deliberately no per-protocol-command wrapper: `trix_call` is a straight
//! pass-through, so a command added to the daemon is usable from the frontend
//! the same day without touching this file. The daemon validates its own
//! arguments and says so in words meant for a person; re-validating here would
//! only produce a second, worse message.

use std::sync::Arc;

use serde_json::{Map, Value};
use tauri::State;

use crate::daemon::Supervisor;

#[tauri::command]
pub async fn trix_call(
    supervisor: State<'_, Arc<Supervisor>>,
    cmd: String,
    args: Map<String, Value>,
) -> Result<Value, String> {
    let supervisor = Arc::clone(&supervisor);
    // spawn_blocking, because `call` parks until the daemon answers and an
    // `arm` takes seconds: doing that on a runtime worker would stall every
    // other command behind it.
    //
    // A `config.set` used to be special-cased right here to re-grant the
    // asset scope on a successful `clip_dir` change — but that only ever ran
    // for a `config.set` this app itself issued, and missed the tray's
    // "Change clips folder…" entirely. The daemon now broadcasts
    // `config_changed` for every accepted `config.set` regardless of who sent
    // it, and `daemon.rs`'s `Connection::start` event closure grants off that
    // instead, so this is a plain pass-through again — one path responsible
    // for the grant, not two.
    tauri::async_runtime::spawn_blocking(move || supervisor.call(&cmd, args))
        .await
        .map_err(|e| format!("the call could not be scheduled: {e}"))?
}

#[tauri::command]
pub fn start_daemon(supervisor: State<'_, Arc<Supervisor>>) -> Result<(), String> {
    supervisor.launch()
}

#[tauri::command]
pub fn daemon_connected(supervisor: State<'_, Arc<Supervisor>>) -> bool {
    supervisor.is_connected()
}
