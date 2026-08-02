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

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("the Tauri runtime failed to start");
}
