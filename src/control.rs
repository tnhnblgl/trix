//! Global hotkey listener (Phase 5b).
//!
//! `RegisterHotKey` binds to the registering thread's message queue, so a
//! dedicated thread owns the registration and pumps messages; each Alt+F10
//! press becomes one `()` on the returned channel.

use std::sync::mpsc::{Receiver, channel};

use anyhow::{Context as _, Result, anyhow};
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_NOREPEAT, RegisterHotKey, VK_F10};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

pub fn start_hotkey() -> Result<Receiver<()>> {
    let (tx, rx) = channel();
    let (ready_tx, ready_rx) = channel::<Result<()>>();
    std::thread::Builder::new()
        .name("trix-hotkey".into())
        .spawn(move || unsafe {
            let registered = RegisterHotKey(None, 1, MOD_ALT | MOD_NOREPEAT, VK_F10.0 as u32);
            let ok = registered.is_ok();
            let _ = ready_tx
                .send(registered.map_err(|e| anyhow!("RegisterHotKey Alt+F10 failed: {e}")));
            if !ok {
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_HOTKEY && tx.send(()).is_err() {
                    return; // receiver gone — session over
                }
            }
        })
        .context("failed to spawn hotkey thread")?;
    ready_rx.recv().context("hotkey thread died during startup")??;
    Ok(rx)
}
