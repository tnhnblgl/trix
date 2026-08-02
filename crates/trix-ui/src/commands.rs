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
    tauri::async_runtime::spawn_blocking(move || {
        let result = supervisor.call(&cmd, args);
        // A clip directory change has to reach the asset scope or the grid
        // renders broken thumbnails from a directory the webview may not read.
        if result.is_ok() && cmd == "config.set" {
            if let Ok(status) = supervisor.call("status", Map::new()) {
                if let Some(dir) = status.get("clip_dir").and_then(Value::as_str) {
                    supervisor.allow_clip_dir(dir);
                }
            }
        }
        result
    })
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
