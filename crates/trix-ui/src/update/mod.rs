//! In-app updates: check, download, verify, swap, restart.
//!
//! The check is automatic; the install never is. Trix's job is to be running,
//! armed and invisible while somebody plays a game, and a background task that
//! stopped the recorder and replaced three executables would do it exactly when
//! the user could least tolerate it. Every entry point here is something the
//! user asked for, and every failure leaves the previous build in place.

pub mod check;
pub mod download;
pub mod swap;
pub mod verify;

use std::sync::Arc;

use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter as _, State};

use crate::daemon::{self, Supervisor};
use check::Release;

/// Keeps a console off the screen when this windowed app spawns one of the
/// console-subsystem binaries beside it.
///
/// The same flag, for the same reason, as `Supervisor::launch`: `trix.exe` is a
/// console binary, so spawning it from here without this flashes a window the
/// user never asked for — once for the version probe, and again for the restart
/// helper, in the middle of an update. A console appearing mid-update does not
/// read as "an update is happening"; it reads as something going wrong.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The subcommand and flag `trix.exe` is relaunched with.
///
/// Named constants rather than three literals buried in the spawn below,
/// because the other half of this pair lives in another crate:
/// `crates/trix-cli/src/main.rs` defines `restart-ui` and `--wait-pid`, and its
/// `restart_ui_parses_the_flag_the_updater_sends` test pins that they still
/// parse. Nothing links the two crates — `trix-ui` cannot depend on `trix-cli`
/// or on `trix-core` (spec §3.2) — so a rename on either side would break the
/// relaunch with both test suites green. These, and the test at the bottom of
/// this file, are what makes the `trix-ui` half a reviewed value that appears
/// once instead of an argument list nobody is watching.
const RESTART_SUBCOMMAND: &str = "restart-ui";
const RESTART_PID_FLAG: &str = "--wait-pid";

/// The exact argument vector handed to `trix.exe`.
fn restart_args(pid: u32) -> [String; 3] {
    [RESTART_SUBCOMMAND.to_string(), RESTART_PID_FLAG.to_string(), pid.to_string()]
}

/// What the frontend renders. `state` is the tag it switches on.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum UpdateState {
    Checking,
    UpToDate,
    Available { release: Release },
    Downloading { received: u64, total: u64 },
    Verifying,
    Installing,
    Restarting,
    Failed { message: String },
}

fn emit(app: &AppHandle, state: &UpdateState) {
    let _ = app.emit("trix-update", state);
}

/// Puts a failure on the same channel the banner is listening to, and hands it
/// back unchanged.
///
/// Wrapped around a whole fallible operation rather than written at each `?`:
/// the two entry points below have a dozen failure points between them, every
/// one of them already produces user-facing copy, and a step added later is
/// covered without anyone having to remember. The `Err` is returned untouched,
/// because the two are not alternatives — the event is how the banner finds
/// out, the `Err` is how the caller does.
///
/// This is the only thing that emits [`UpdateState::Failed`], and it matters
/// most for `update_install`, which returns *only* on failure: by then the user
/// has been watching a progress bar driven entirely by these events, and a
/// rejected promise with nothing on the event channel would leave that bar
/// sitting where it stopped.
fn reported<T>(app: &AppHandle, result: Result<T, String>) -> Result<T, String> {
    if let Err(message) = &result {
        emit(app, &UpdateState::Failed { message: message.clone() });
    }
    result
}

/// Clears away the previous build, and repairs an update that was interrupted.
///
/// Hands the install directory to [`swap::cleanup`], which is where the
/// load-bearing half lives. Deleting the `.old` copies and the staging folder
/// is the ordinary case; the branch that matters is the other one. Where a
/// binary's live name is *missing* and only its `.old` copy exists, that copy
/// is renamed back rather than deleted — [`swap::swap_in`] cannot leave the
/// install in that state, because its rollback closes the gap, but something
/// that stops the process outright between two renames can, and then the
/// `.old` file is the only copy of that program in existence.
///
/// A restore is not the same as a completed update, and this function has no
/// way to say so: any binary earlier in [`swap::BINARIES`] has already moved to
/// the new build, and the same pass deletes the staging folder the rest of the
/// payload was sitting in, so the install is left mixed-version and the update
/// has to be run again. A vacated `trix-ui.exe` cannot be repaired here at all,
/// since this only ever runs from inside `trix-ui.exe`. `swap::cleanup`'s own
/// doc has the full account.
///
/// Called once at startup, before the window opens and before the daemon
/// supervisor starts — the restore branch is the only thing that puts back a
/// `trix-daemon.exe` an interrupted update renamed away, and a supervisor
/// started first would be handed a moment where that binary is genuinely gone.
pub fn clean_up_after_update() {
    if let Ok(dir) = swap::install_dir() {
        swap::cleanup(&dir);
    }
}

#[tauri::command]
pub fn update_current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// `Ok(None)` is the common answer and is not shown to the user unless they
/// asked. A failed check returns `Err` and emits [`UpdateState::Failed`]; the
/// frontend surfaces either only for a manual "Check now" and discards both for
/// an automatic one, because the user did not ask to check for updates, they
/// asked to open a clip recorder.
///
/// Discarded really does mean gone. `trix-ui` carries no logger, and under
/// `windows_subsystem = "windows"` there is no console to print to either, so
/// the returned `Err` and the emitted event are the only two places a failed
/// check exists at all — which is also why both are produced rather than one.
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Option<Release>, String> {
    // Cloned before the move so the join failure has somewhere to be reported
    // too. `Checking` has already been emitted by then, and a task that cannot
    // be joined emits nothing of its own, so without this the banner sits on
    // "Checking…" for as long as the window is open.
    let scheduling = app.clone();
    let joined = tauri::async_runtime::spawn_blocking(move || {
        emit(&app, &UpdateState::Checking);
        reported(&app, look_for_a_newer_release(&app))
    })
    .await
    .map_err(|e| format!("the update check could not be scheduled: {e}"));
    // Two layers: the outer is whether the task ran at all, the inner is what
    // it decided. Only the outer is reported here -- the inner has already been
    // through `reported` inside the closure, and doing it twice would put two
    // `Failed` events on the channel for one failure.
    reported(&scheduling, joined)?
}

fn look_for_a_newer_release(app: &AppHandle) -> Result<Option<Release>, String> {
    let body = download::fetch_text(check::RELEASE_API)?;
    let found = check::newer_release(&body, env!("CARGO_PKG_VERSION"))?;
    match &found {
        Some(release) => emit(app, &UpdateState::Available { release: release.clone() }),
        None => emit(app, &UpdateState::UpToDate),
    }
    Ok(found)
}

/// Downloads, verifies, stops the daemon, swaps, and restarts.
///
/// Returns only on failure: on success the process is replaced by a newly
/// spawned one and this one exits.
#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    supervisor: State<'_, Arc<Supervisor>>,
    release: Release,
) -> Result<(), String> {
    let supervisor = Arc::clone(&supervisor);
    // Same shape as `update_check`, and it matters more here: this command
    // returns only on failure, so a join error with nothing on the event
    // channel would leave the progress bar sitting exactly where it stopped
    // with no explanation beside it.
    let scheduling = app.clone();
    let joined = tauri::async_runtime::spawn_blocking(move || {
        reported(&app, install(&app, &supervisor, &release))
    })
    .await
    .map_err(|e| format!("the update could not be scheduled: {e}"));
    reported(&scheduling, joined)?
}

fn install(app: &AppHandle, supervisor: &Supervisor, release: &Release) -> Result<(), String> {
    // Where the files come from, before a byte of them is fetched. `release`
    // is a command argument, so it is whatever the webview passed in, not
    // necessarily what `update_check` built -- and the host allowlist further
    // down cannot tell this repository's releases from anyone else's, because
    // both are `github.com`.
    release.assets_are_ours()?;

    // Then the preflight, in the order that costs least to refuse. Recording
    // first: a user mid-session must be told to stop, not have the recorder
    // pulled out from under them.
    //
    // Asked only while the socket is actually up, because a socket is the only
    // way to ask it at all: "armed" is the daemon's own answer about its own
    // state, not something observable from out here. The app is fully usable
    // with the daemon down -- that is what spec §4.5's "not running" panel is
    // for, and it is also the state a user is in when the daemon is the thing
    // that is broken. Letting a disconnected socket answer here would make the
    // one build that could fix that the one build that cannot be installed,
    // and it would refuse with "not connected to the Trix daemon", which says
    // nothing about the update the user just asked for.
    //
    // `is_connected` is not the same question as "is a daemon running", and
    // this is the only place it is allowed to stand in for it. The supervisor
    // reconnects with backoff, so a perfectly live daemon reads as
    // disconnected for seconds at a time and this check is simply skipped
    // then. The swap guard below must not be skipped in that window, so it
    // asks `pipe_exists` instead, and it re-asks this one on the way past.
    if supervisor.is_connected() {
        let status = supervisor.call("status", Map::new())?;
        if status.get("armed").and_then(Value::as_bool) == Some(true) {
            return Err("Stop recording before updating Trix.".into());
        }
    }

    let dir = swap::install_dir()?;
    swap::writable(&dir)?;

    let staging = dir.join(swap::STAGING);
    // A staging folder from an update that failed before cleanup ran.
    let _ = std::fs::remove_dir_all(&staging);

    let zip_name = format!("trix-v{}-win-x64.zip", release.version);
    let zip_path = staging.join("download").join(&zip_name);

    let progress_app = app.clone();
    download::fetch_to_file(&release.zip_url, &zip_path, &move |received, total| {
        emit(&progress_app, &UpdateState::Downloading { received, total });
    })?;

    emit(app, &UpdateState::Verifying);
    let sums = download::fetch_text(&release.sums_url)?;
    verify::check(&zip_path, &sums, &zip_name)?;

    let payload = swap::unpack(&zip_path, &staging.join("staged"))?;
    verify_payload_version(&payload, &release.version)?;

    emit(app, &UpdateState::Installing);
    // `pipe_exists`, not `is_connected`: the question here is whether a daemon
    // is *running*, and a running daemon this app has momentarily lost its
    // socket to is still running. Getting that wrong is silent rather than
    // loud, which is what makes it worth the extra care -- Windows lets a
    // running executable be renamed (the loader opens images with
    // FILE_SHARE_DELETE), so `swap_in` would succeed, report success, and
    // leave the old daemon executing out of `trix-daemon.exe.old` with the new
    // UI talking to it. `swap::cleanup` cannot delete a mapped image either,
    // so the next launch would not fix it, and the user would be told they had
    // been updated while the capture daemon was the previous build.
    if daemon::pipe_exists() {
        // The preflight above could not ask this if the socket happened to be
        // down when the user pressed Install. By now it may well be up, and
        // this is the last moment before the recorder is shut down under
        // whatever it was doing. A call that fails tells us nothing either way
        // and is left to `stop` below, which reports the real problem.
        if let Ok(status) = supervisor.call("status", Map::new())
            && status.get("armed").and_then(Value::as_bool) == Some(true)
        {
            return Err("Stop recording before updating Trix.".into());
        }

        // A daemon can leave between the check above and this call -- an idle
        // one exiting on its own, the user quitting it from the tray -- and
        // `stop` would then fail for want of anything to stop. Asking again is
        // what tells the two apart: nothing owns the pipe any more, so there
        // was nothing left to stop and the update carries on; something still
        // does, so the failure is real and swapping under it is exactly what
        // this guard exists to prevent.
        if let Err(e) = supervisor.stop()
            && daemon::pipe_exists()
        {
            return Err(format!(
                "{e}. The Trix recorder has to stop before its program can be replaced, so \
                 nothing was changed."
            ));
        }
    }
    // `swap_in`'s message, passed through exactly as it comes. It already ends
    // by telling the user where they stand, and it says two different things:
    // "the version you were running is still installed" when the rollback
    // worked, and a repair procedure -- including the warning that dropping a
    // ".old" suffix can overwrite a working file rather than fill a gap --
    // when it did not. Appending anything about Trix being unchanged would
    // contradict the second, and it would be the last thing the user reads.
    swap::swap_in(&dir, &payload)?;

    emit(app, &UpdateState::Restarting);
    let helper = dir.join("trix.exe");
    use std::os::windows::process::CommandExt as _;
    std::process::Command::new(&helper)
        .args(restart_args(std::process::id()))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        // Everything is already in place by the time this can fail, so the
        // message says so: the user has the new version, and the only thing
        // left undone is the part they can do themselves.
        .map_err(|e| {
            format!(
                "Trix was updated but could not restart itself: {e}. Close Trix and start it \
                 again to use the new version."
            )
        })?;

    app.exit(0);
    Ok(())
}

/// Asks the staged `trix.exe` what version it is.
///
/// The same question `ship-zip.ps1` asks before it will write a zip, asked
/// again before installing. It costs one process spawn and it is the only check
/// that compares the *binaries* against the version being promised, rather than
/// comparing one piece of metadata with another.
fn verify_payload_version(payload: &std::path::Path, expected: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt as _;

    let output = std::process::Command::new(payload.join("trix.exe"))
        .arg("--version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("the downloaded update could not be checked: {e}"))?;
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if reported == format!("trix {expected}") {
        Ok(())
    } else {
        Err(format!(
            "the downloaded update reports itself as {reported:?} rather than trix {expected}, \
             so it was not installed"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A release shaped like one `check::newer_release` produces.
    fn a_release() -> Release {
        const RELEASES: &str = "https://github.com/tnhnblgl/trix/releases";
        Release {
            version: "0.5.0".to_string(),
            notes_url: format!("{RELEASES}/tag/v0.5.0"),
            zip_url: format!("{RELEASES}/download/v0.5.0/trix-v0.5.0-win-x64.zip"),
            sums_url: format!("{RELEASES}/download/v0.5.0/SHA256SUMS.txt"),
            size: 3_400_000,
        }
    }

    /// The frontend switches on `state`, so these strings are a contract with
    /// the webview, not debug output. Renaming one silently stops a banner
    /// rendering.
    #[test]
    fn every_state_serialises_under_the_name_the_frontend_matches() {
        let cases = [
            (UpdateState::Checking, "checking"),
            (UpdateState::UpToDate, "up-to-date"),
            (UpdateState::Available { release: a_release() }, "available"),
            (UpdateState::Downloading { received: 1, total: 2 }, "downloading"),
            (UpdateState::Verifying, "verifying"),
            (UpdateState::Installing, "installing"),
            (UpdateState::Restarting, "restarting"),
            (UpdateState::Failed { message: "x".into() }, "failed"),
        ];
        for (state, name) in cases {
            let json = serde_json::to_value(&state).expect("serialises");
            assert_eq!(json.get("state").and_then(|v| v.as_str()), Some(name));
        }
    }

    #[test]
    fn downloading_carries_both_numbers_the_progress_bar_needs() {
        let json = serde_json::to_value(UpdateState::Downloading { received: 40, total: 100 })
            .expect("serialises");
        assert_eq!(json.get("received").and_then(|v| v.as_u64()), Some(40));
        assert_eq!(json.get("total").and_then(|v| v.as_u64()), Some(100));
    }

    /// `update_check` hands the frontend a `Release` and `update_install` takes
    /// one straight back, so the struct has to survive a round trip through
    /// JavaScript. Serialising it is not enough on its own -- without the
    /// matching `Deserialize`, `update_install` does not compile at all, and
    /// with only one side of the pair kept in step a renamed field would land
    /// as an "invalid args" string from Tauri rather than as anything the
    /// banner could explain.
    #[test]
    fn a_release_survives_the_round_trip_through_the_webview() {
        let release = a_release();
        let json = serde_json::to_string(&release).expect("serialises");
        let back: Release = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, release, "what the webview hands back must be what it was given");

        // Spelled out as literals rather than derived from the struct, which
        // is the only way a test can notice a renamed field: a round trip
        // through this crate's own `Serialize`/`Deserialize` agrees with
        // itself whatever the names are, while the banner is written against
        // these five strings and would quietly stop finding one.
        let value: Value = serde_json::from_str(&json).expect("is JSON");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("a Release is a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["notes_url", "size", "sums_url", "version", "zip_url"],
            "these are the field names the webview reads; renaming one is a frontend change too"
        );
    }

    /// The command line `trix.exe` is relaunched with, pinned from this side.
    ///
    /// `crates/trix-cli/src/main.rs` pins that `trix.exe` parses exactly this;
    /// nothing links the two crates, so without this test the `trix-ui` half
    /// could be changed with every suite in the workspace still green and the
    /// only symptom an updated install with no running app.
    #[test]
    fn the_relaunch_sends_the_command_line_trix_exe_parses() {
        assert_eq!(
            restart_args(4321),
            ["restart-ui", "--wait-pid", "4321"],
            "trix-cli's restart_ui_parses_the_flag_the_updater_sends is the other half of this"
        );
    }
}
