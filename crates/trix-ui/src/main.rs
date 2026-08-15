//! `trix-ui`: the Trix desktop app.
//!
//! Its Rust half is a named-pipe client and a window, and that is the whole
//! design. Every capability this app has — arming, clipping, listing, deleting,
//! settings — arrives over the control socket, because spec §3.2 forbids this
//! crate from depending on `trix-core`. That rule is not stylistic: it is the
//! only thing that keeps the protocol honest enough for a UI somebody else
//! writes to be a first-class client.

// No console window behind the app in release. Debug keeps one, because
// `tracing`-style eprintln debugging of the socket is the whole reason to run
// a debug build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod daemon;
mod pipe;
mod pipe_reader;
mod update;

use tauri::Manager as _;

fn main() {
    tauri::Builder::default()
        // Single instance first, before anything expensive: the second copy's
        // whole job is to hand focus to the first and exit. The tray's "Open
        // Trix" (Task 11) runs the exe unconditionally, so this is what makes
        // clicking it twice raise one window instead of opening two.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let supervisor = daemon::Supervisor::start(app.handle().clone());
            app.manage(supervisor);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::trix_call,
            commands::start_daemon,
            commands::daemon_connected,
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime failed to start");
}
