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
    // Refused for the length of an install, and refused here rather than by
    // hiding the button. An update stops the daemon before it renames
    // `trix-daemon.exe`, and the supervisor emits `trix-disconnected` the
    // moment it does -- so for those few seconds the window is showing spec
    // §4.5's "not running" panel, whose entire content is an offer to start
    // the thing the updater just stopped. Taking that offer launches the old
    // daemon into the gap `update::install`'s `pipe_exists` guard just
    // cleared, and Windows lets a running executable be renamed: the swap
    // would report success and leave the previous build executing out of
    // `trix-daemon.exe.old`, where the next launch's cleanup cannot delete it
    // either.
    //
    // The updater puts the recorder back itself when it was the thing that
    // stopped it and the install left a `trix-daemon.exe` fit to run. It does
    // not always manage both -- an update that fails because the recorder will
    // not stop puts nothing back, and neither does one whose rollback strands
    // the daemon binary. What holds in every case is that this refusal lasts
    // no longer than the install does: the flag is given up on the way out
    // however the install ended, so the offer on that panel is live again by
    // the time the user can act on it.
    if crate::update::install_in_progress() {
        return Err("Trix is installing an update, so the recorder cannot be started right now. \
                    Try again once the update has finished."
            .to_string());
    }
    supervisor.launch()
}

#[tauri::command]
pub fn daemon_connected(supervisor: State<'_, Arc<Supervisor>>) -> bool {
    supervisor.is_connected()
}
