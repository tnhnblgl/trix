//! The daemon's one hidden window, its message pump, and the clip hotkey.
//!
//! Two Win32 facilities the daemon needs are bound to a window and a message
//! queue: `Shell_NotifyIconW` (Task 8) delivers tray callbacks to an `HWND`,
//! and `RegisterHotKey` posts `WM_HOTKEY` to the registering *thread's* queue.
//! Both are served by the single message-only window created here, so stage 3
//! adds one thread rather than two.
//!
//! **The pump thread never calls into `Daemon`.** `Daemon::clip` blocks for a
//! multi-second mux and `Daemon::arm` for a multi-second capture startup; a
//! window that stops pumping stops redrawing the tray, stops answering
//! `WM_ENDSESSION`, and gets marked "not responding" by the OS. Every action
//! is therefore a message on a channel that the worker thread drains.

use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TrySendError};

use anyhow::{Context as _, Result};
use trix_core::control::Hotkey;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_HOTKEY, WNDCLASSW,
};
use windows::core::HSTRING;

use crate::state::Daemon;

/// What the pump thread asks the worker to do. Deliberately tiny: anything
/// that can block belongs on the worker side of this channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Hotkey pressed, or "Save clip" chosen from the tray menu.
    Clip,
    Arm,
    Disarm,
    /// Tray menu: toggle depending on current state.
    ToggleArmed,
    OpenClipsFolder,
    Quit,
}

/// How many pending actions the pump may hold. Small on purpose: these are
/// human-scale events, and a deep queue would only mean replaying a backlog of
/// stale clicks after a slow operation finally returns.
pub const ACTION_QUEUE_DEPTH: usize = 4;

/// Hands an action to the worker without ever blocking the pump thread.
/// Returns false if the action was dropped — a full queue or a dead worker.
pub(crate) fn offer(tx: &SyncSender<Action>, action: Action) -> bool {
    match tx.try_send(action) {
        Ok(()) => true,
        Err(TrySendError::Full(dropped)) => {
            tracing::warn!(?dropped, "action queue full; dropping (an operation is still running)");
            false
        }
        Err(TrySendError::Disconnected(dropped)) => {
            tracing::debug!(?dropped, "worker is gone; dropping");
            false
        }
    }
}

/// Posted to the window to ask its pump to exit.
pub(crate) const WM_TRIX_QUIT: u32 = WM_APP + 0x10;
/// The hotkey id. Process-unique is enough — the window owns the only one.
const HOTKEY_ID: i32 = 1;

/// A live pump thread and the window it owns.
pub struct WindowHandle {
    hwnd: isize,
    join: Option<std::thread::JoinHandle<()>>,
}

impl WindowHandle {
    /// The window every tray call attaches to. Stored as `isize` because
    /// `HWND` is not `Send`; the handle itself is process-wide and valid from
    /// any thread for `PostMessageW` and `Shell_NotifyIconW`.
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut core::ffi::c_void)
    }

    /// Asks the pump to exit and joins it. Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd()), WM_TRIX_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = join.join();
        }
    }
}

impl Drop for WindowHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

thread_local! {
    static ACTIONS: std::cell::RefCell<Option<SyncSender<Action>>> =
        const { std::cell::RefCell::new(None) };
}

/// Never blocks and never calls into `Daemon` — see the module comment.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
            ACTIONS.with(|a| {
                if let Some(tx) = a.borrow().as_ref() {
                    offer(tx, Action::Clip);
                }
            });
            LRESULT(0)
        }
        WM_TRIX_QUIT => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // Task 8 adds the tray callback arm here.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Starts the pump thread, creates the window, and registers `clip_hotkey`.
///
/// Returns once the window exists, so a caller can attach a tray icon to it
/// immediately. A hotkey that fails to register is a **warning, not an
/// error**: another program owning Alt+F10 (NVIDIA's overlay does exactly
/// this) must not stop the daemon from starting — the user can still clip from
/// the tray and can rebind in settings.
pub fn spawn(actions: SyncSender<Action>, hotkey_spec: &str) -> Result<WindowHandle> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<isize>>();
    let hotkey = Hotkey::parse(hotkey_spec);
    let spec = hotkey_spec.to_string();

    let join = std::thread::Builder::new()
        .name("trix-window".into())
        .spawn(move || {
            ACTIONS.with(|a| *a.borrow_mut() = Some(actions));
            let created = unsafe { create_window() };
            let hwnd = match created {
                Ok(hwnd) => {
                    let _ = ready_tx.send(Ok(hwnd.0 as isize));
                    hwnd
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };

            let registered = match &hotkey {
                Ok(hk) => match unsafe { hk.register(hwnd, HOTKEY_ID) } {
                    Ok(()) => {
                        tracing::info!(hotkey = %hk, "clip hotkey registered");
                        true
                    }
                    Err(e) => {
                        tracing::warn!(hotkey = %hk, error = %format!("{e:#}"), "clip hotkey unavailable — clip from the tray, or rebind clip_hotkey");
                        false
                    }
                },
                Err(e) => {
                    tracing::warn!(spec = %spec, error = %format!("{e:#}"), "clip_hotkey is not parseable; no hotkey registered");
                    false
                }
            };

            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            if registered {
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                }
            }
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        })
        .context("failed to spawn the window thread")?;

    let hwnd = ready_rx.recv().context("window thread died during startup")??;
    Ok(WindowHandle { hwnd, join: Some(join) })
}

/// Registers the class (idempotent per process) and creates a message-only
/// window. `HWND_MESSAGE` gives a window with no taskbar presence, no paint
/// cycle, and no z-order — it exists purely to receive messages, which is
/// exactly what the hotkey and the tray need.
unsafe fn create_window() -> Result<HWND> {
    let class = HSTRING::from("TrixDaemonWindow");
    let instance = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW")?;
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        lpszClassName: windows::core::PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    // A zero return is "already registered" on the second daemon in a process
    // — harmless, and there is only ever one.
    unsafe { RegisterClassW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::PCWSTR(class.as_ptr()),
            &HSTRING::from("Trix"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    }
    .context("CreateWindowExW")?;
    Ok(hwnd)
}

/// Runs one action. Called on the worker thread, where blocking is fine.
pub fn handle_action(daemon: &Arc<Daemon>, action: Action) {
    match action {
        Action::Clip => match daemon.clip() {
            Ok(Some(meta)) => tracing::info!(clip = %meta.id, "clip saved from the tray or hotkey"),
            // `arm` only starts filling the ring when the first frame arrives,
            // so a hotkey pressed in the first moments after arming is a real
            // "nothing buffered yet" rather than a failure.
            Ok(None) => tracing::warn!("nothing buffered yet — no clip saved"),
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "clip failed"),
        },
        Action::Arm | Action::Disarm | Action::ToggleArmed => {
            // Task 8 fills these in with the tray's toggle semantics.
            tracing::debug!(?action, "not wired until the tray lands");
        }
        Action::OpenClipsFolder | Action::Quit => {
            tracing::debug!(?action, "not wired until the tray lands");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;

    use super::*;

    /// The pump must never block on a full action queue. A user mashing the
    /// hotkey during a slow mux would otherwise freeze the tray icon and the
    /// window with it — the exact failure this design exists to prevent.
    /// Dropping a duplicate clip request is the correct loss: the clip already
    /// in flight covers the same moment.
    #[test]
    fn a_full_action_queue_drops_instead_of_blocking() {
        let (tx, rx) = sync_channel::<Action>(ACTION_QUEUE_DEPTH);
        for _ in 0..ACTION_QUEUE_DEPTH {
            assert!(offer(&tx, Action::Clip), "queue must accept up to its depth");
        }
        // The queue is full and nothing is draining it. This must return
        // immediately rather than parking the caller.
        let started = std::time::Instant::now();
        assert!(!offer(&tx, Action::Clip), "an overfull queue must report the drop");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "offer blocked for {:?} — the pump thread would have hung",
            started.elapsed()
        );
        drop(rx);
    }

    /// A disconnected worker must also not block or panic: the worker exits
    /// first on shutdown, and the pump may still be draining a final click.
    #[test]
    fn offering_to_a_dead_worker_is_a_drop_not_a_panic() {
        let (tx, rx) = sync_channel::<Action>(ACTION_QUEUE_DEPTH);
        drop(rx);
        assert!(!offer(&tx, Action::Quit));
    }

    /// A hotkey the daemon cannot have must not stop the daemon. NVIDIA's
    /// overlay owns Alt+F10 — the configured default — on the developer's own
    /// machine, so this is the common case, not the corner case: the user
    /// still needs a running daemon to clip from the tray and to rebind
    /// `clip_hotkey` from settings.
    ///
    /// Uses an unparseable spec rather than a taken combination, because the
    /// alternative is a test that steals a real global hotkey from the
    /// developer's desktop for as long as it runs.
    #[test]
    fn a_hotkey_that_cannot_be_registered_still_leaves_a_live_window() {
        let (tx, _rx) = sync_channel::<Action>(ACTION_QUEUE_DEPTH);
        let mut window =
            spawn(tx, "not+a+hotkey").expect("an unusable hotkey must not fail the daemon");
        assert!(!window.hwnd().0.is_null(), "the window must exist even with no hotkey");
        // Must return rather than hang: the pump is a live thread, and the
        // only thing that ends it is this message.
        window.shutdown();
    }
}
