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
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};

use anyhow::{Context as _, Result};
use trix_core::control::Hotkey;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
};
use windows::core::HSTRING;

use crate::state::Daemon;
use crate::tray::{self, Tray};

/// What the tray icon is currently showing, and what the menu's toggle item
/// reads. The pump reads this instead of taking `Daemon`'s `armed` mutex —
/// that mutex is held across engine startup and teardown, and blocking the
/// pump on it is exactly what this module's comment forbids.
static ARMED_MIRROR: AtomicBool = AtomicBool::new(false);

/// The pump's window, for code that is not on the pump thread. Zero until the
/// window exists, which is what makes [`publish_armed`] a no-op in unit tests
/// and in `trix.exe`, neither of which has a tray.
static WINDOW_HWND: AtomicIsize = AtomicIsize::new(0);

/// The `TaskbarCreated` broadcast id, resolved once the window thread starts.
static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
/// Posted to the window when the daemon arms or disarms; `wparam` is the new
/// state. The icon is only ever changed on the thread that owns it.
pub(crate) const WM_TRIX_ARMED: u32 = WM_APP + 0x11;
/// The hotkey id. Process-unique is enough — the window owns the only one.
const HOTKEY_ID: i32 = 1;

/// A live pump thread and the window it owns.
pub struct WindowHandle {
    hwnd: isize,
    join: Option<std::thread::JoinHandle<()>>,
    /// Set by the pump once the tray icon has been removed and the window
    /// destroyed. The shutdown path waits on this rather than on the thread,
    /// because it has a time budget and `join` has no timeout.
    finished: Arc<AtomicBool>,
}

impl WindowHandle {
    /// The window every tray call attaches to. Stored as `isize` because
    /// `HWND` is not `Send`; the handle itself is process-wide and valid from
    /// any thread for `PostMessageW` and `Shell_NotifyIconW`.
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut core::ffi::c_void)
    }

    /// Asks the pump to exit and returns immediately.
    ///
    /// Separate from [`shutdown`](Self::shutdown) for the shutdown watcher,
    /// which wants the tray icon to start disappearing *now* — before the
    /// multi-second disarm — but cannot afford an unbounded join before
    /// `process::exit`.
    pub fn request_exit(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_TRIX_QUIT, WPARAM(0), LPARAM(0));
        }
    }

    /// Waits up to `timeout` for the icon to be gone and the window destroyed.
    /// Returns false on timeout, which is a reason to log, not to hang.
    pub fn wait_for_exit(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.finished.load(Ordering::Acquire) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.finished.load(Ordering::Acquire)
    }

    /// Asks the pump to exit and joins it. Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            self.request_exit();
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
    /// Lives here rather than in `main` because a tray icon belongs to the
    /// thread that owns its window: `Shell_NotifyIconW` posts callbacks to
    /// that thread, and `Tray`'s `Drop` must run there too.
    static TRAY: std::cell::RefCell<Option<Tray>> = const { std::cell::RefCell::new(None) };
}

/// Records the armed state and asks the pump to repaint the tray icon.
///
/// Called from wherever the state actually changes — `Daemon::arm` and
/// `Daemon::disarm` — rather than from the tray's own handler, so the icon is
/// still correct when something else arms the daemon: the control socket
/// today, the desktop UI in stage 4. A tray icon that only tracks its own menu
/// would show "idle" through a UI-initiated recording.
pub fn publish_armed(armed: bool) {
    ARMED_MIRROR.store(armed, Ordering::Relaxed);
    let hwnd = WINDOW_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return; // no window: unit tests, and `trix.exe`
    }
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            WM_TRIX_ARMED,
            WPARAM(usize::from(armed)),
            LPARAM(0),
        );
    }
}

/// Never blocks and never calls into `Daemon` — see the module comment.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Not a `match` arm because the message id is assigned at runtime by
    // `RegisterWindowMessageW` and match patterns must be constants.
    if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) {
        TRAY.with(|t| {
            if let Some(tray) = t.borrow().as_ref() {
                tray.readd();
            }
        });
        return LRESULT(0);
    }
    match msg {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
            ACTIONS.with(|a| {
                if let Some(tx) = a.borrow().as_ref() {
                    offer(tx, Action::Clip);
                }
            });
            LRESULT(0)
        }
        WM_TRIX_ARMED => {
            let armed = wparam.0 != 0;
            TRAY.with(|t| {
                if let Some(tray) = t.borrow_mut().as_mut() {
                    tray.set_armed(armed);
                }
            });
            LRESULT(0)
        }
        tray::WM_TRIX_TRAY => {
            // Without NOTIFYICON_VERSION_4 (which we do not ask for) the
            // callback's lparam is the mouse message itself.
            let action = match lparam.0 as u32 {
                // Left click opens the UI. Until stage 4 ships one, that is
                // the clips folder — the thing the user came to look at.
                WM_LBUTTONUP => Some(Action::OpenClipsFolder),
                WM_RBUTTONUP => {
                    // The menu is modal and pumps its own messages, but it is
                    // driven by the user and returns promptly, so it is the
                    // one thing this thread is allowed to do inline.
                    let armed = ARMED_MIRROR.load(Ordering::Relaxed);
                    match unsafe { tray::show_menu(hwnd, armed) } {
                        Some(tray::ID_TOGGLE) => Some(Action::ToggleArmed),
                        Some(tray::ID_OPEN_UI) => Some(Action::OpenClipsFolder),
                        Some(tray::ID_OPEN_FOLDER) => Some(Action::OpenClipsFolder),
                        Some(tray::ID_QUIT) => Some(Action::Quit),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(action) = action {
                ACTIONS.with(|a| {
                    if let Some(tx) = a.borrow().as_ref() {
                        offer(tx, action);
                    }
                });
            }
            LRESULT(0)
        }
        WM_TRIX_QUIT => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
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
    let finished = Arc::new(AtomicBool::new(false));
    let pump_finished = Arc::clone(&finished);

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
            WINDOW_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
            // A message-only window cannot be found with `FindWindowEx` from
            // another process, so this line is the only way anything outside
            // the daemon can name it — which is what diagnosing "the tray
            // icon never appeared" needs.
            tracing::debug!(hwnd = hwnd.0 as isize, "message-only window created");

            // Registered before the icon is added, so a shell that restarts
            // during startup is still caught.
            TASKBAR_CREATED.store(
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(
                        &HSTRING::from("TaskbarCreated"),
                    )
                },
                Ordering::Relaxed,
            );
            match unsafe { Tray::add(hwnd) } {
                Ok(tray) => TRAY.with(|t| *t.borrow_mut() = Some(tray)),
                // The daemon is still fully usable over the pipe and the
                // hotkey without an icon, so this is not worth refusing to
                // start over — same call as the hotkey below.
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "no tray icon; the daemon is still running")
                }
            }

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
            // Before `DestroyWindow`, and on this thread: `Tray::drop` sends
            // `NIM_DELETE` to the shell for this window. An icon whose window
            // is already gone is the one that lingers in the tray until the
            // user hovers it, which is what a crashed app looks like.
            TRAY.with(|t| drop(t.borrow_mut().take()));
            WINDOW_HWND.store(0, Ordering::Relaxed);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            pump_finished.store(true, Ordering::Release);
        })
        .context("failed to spawn the window thread")?;

    let hwnd = ready_rx.recv().context("window thread died during startup")??;
    Ok(WindowHandle { hwnd, join: Some(join), finished })
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
        // The mirror decides what the menu offered, so a toggle does what the
        // user just read, not what the daemon became a moment later.
        Action::ToggleArmed => set_armed(daemon, !ARMED_MIRROR.load(Ordering::Relaxed)),
        Action::Arm => set_armed(daemon, true),
        Action::Disarm => set_armed(daemon, false),
        Action::OpenClipsFolder => {
            let dir = daemon.clip_dir();
            // `explorer.exe` returns a non-zero exit code even on success, so
            // its status is deliberately ignored rather than logged as a
            // failure the user would see in the log for no reason.
            let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
            tracing::debug!(dir = %dir.display(), "opened the clips folder");
        }
        Action::Quit => {
            // The same path Ctrl+C takes, so a tray Quit finalizes an
            // in-flight mux exactly like a console close does rather than
            // dropping the clip the user just asked for.
            tracing::info!("quit requested from the tray");
            trix_core::control::request_shutdown();
        }
    }
}

/// Arms or disarms, and lets `Daemon` publish the result. Both calls are
/// multi-second, which is why this only ever runs on the worker thread.
fn set_armed(daemon: &Arc<Daemon>, armed: bool) {
    let result = if armed { daemon.arm().map(|_| ()) } else { daemon.disarm().map(|_| ()) };
    if let Err(e) = result {
        let verb = if armed { "arm" } else { "disarm" };
        tracing::warn!(error = %format!("{e:#}"), "the tray could not {verb} the daemon");
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
