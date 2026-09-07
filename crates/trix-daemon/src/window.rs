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
//!
//! The one exception is `Clients::broadcast`, which `wnd_proc`'s `WM_HOTKEY`
//! arm calls directly. That is not the same invariant as calling into
//! `Daemon`: `broadcast` is `try_send`-based against a handful of small
//! bounded queues (see `clients.rs`) and cannot block on a slow capture or a
//! stuck disk the way `Daemon::clip`/`arm` can — it is a mailbox drop, not a
//! multi-second operation with a lock held across it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};
use trix_core::config::HotkeyMode;
use trix_core::control::Hotkey;
use trix_proto::Event;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
};
use windows::core::HSTRING;

use crate::clients::Clients;
use crate::hook;
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

/// Whether the hotkeys are currently bound through the low-level keyboard hook
/// rather than the system hotkey table — `hotkey_mode` in config.toml, as a
/// single bit.
///
/// Global and atomic for the same reason [`WINDOW_HWND`] is: `set_hotkey_mode`
/// is called on the socket's worker thread while only the pump may touch a
/// registration. The atomic carries the decision across and
/// `WM_TRIX_REHOTKEY` carries the work, so nothing but the pump ever calls
/// `RegisterHotKey` or `SetWindowsHookEx`.
static LOW_LEVEL_HOTKEYS: AtomicBool = AtomicBool::new(false);

/// Whether a hotkey is currently listening, as a **tri-state**: nobody has
/// tried yet, it is bound, or something else has the combination.
///
/// Three states rather than a bool because "not bound" and "not asked yet" are
/// different answers and only one of them is worth warning a user about.
/// `spawn` returns as soon as the window exists and binds a moment later on
/// the pump thread, so a `status` answered in that window has genuinely not
/// been told yet — reported as `false` it would flash "your hotkey is taken"
/// at every launch, which is the boy-who-cried-wolf version of the warning
/// this exists to give.
const HOTKEY_UNTRIED: u8 = 0;
const HOTKEY_BOUND: u8 = 1;
const HOTKEY_TAKEN: u8 = 2;
static CLIP_HOTKEY_STATE: AtomicU8 = AtomicU8::new(HOTKEY_UNTRIED);
static SHOT_HOTKEY_STATE: AtomicU8 = AtomicU8::new(HOTKEY_UNTRIED);

fn hotkey_state(kind: HotkeyKind) -> &'static AtomicU8 {
    match kind {
        HotkeyKind::Clip => &CLIP_HOTKEY_STATE,
        HotkeyKind::Screenshot => &SHOT_HOTKEY_STATE,
    }
}

/// Whether `kind` is listening right now — `None` until the pump has tried.
///
/// This is the whole of the answer to "why does my hotkey do nothing?". Before
/// it existed, a registration Windows refused produced one `tracing::warn!`
/// into a log with no file behind it, and the user's entire experience was a
/// key that silently did nothing forever. `status` carries it now, so Settings
/// can say which combination was refused and what to do about it.
pub fn hotkey_bound(kind: HotkeyKind) -> Option<bool> {
    decode_hotkey_state(hotkey_state(kind).load(Ordering::Acquire))
}

/// The tri-state rule on its own, away from the statics that hold it.
///
/// Split out to be testable: the statics are process-global, so a test that
/// asserted on them would be reading whatever the pump tests in this same
/// binary had last written. This is the part that must never get the safety
/// direction wrong — an untried hotkey reported as `Some(false)` is a false
/// "your hotkey is taken" on every client that connects during startup.
fn decode_hotkey_state(raw: u8) -> Option<bool> {
    match raw {
        HOTKEY_BOUND => Some(true),
        HOTKEY_TAKEN => Some(false),
        _ => None,
    }
}

/// What the pump thread asks the worker to do. Deliberately tiny: anything
/// that can block belongs on the worker side of this channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The clip hotkey was pressed (`WM_HOTKEY`). The tray menu has no "save
    /// clip" item of its own — see its match arms below for what it does map
    /// to — so this is never reached any other way.
    Clip,
    /// The screenshot hotkey was pressed. Like [`Self::Clip`] this has no tray
    /// menu item, so `WM_HOTKEY` is the only thing that produces it.
    Screenshot,
    Arm,
    Disarm,
    /// Tray menu: toggle depending on current state.
    ToggleArmed,
    /// Tray menu's "Open Trix" and a left click on the icon: launch the
    /// desktop app, or fall back to the clips folder if it is not installed.
    OpenApp,
    OpenClipsFolder,
    /// Pick a new clips folder — the tray menu, or the settings page over
    /// `folder.pick`. Both arrive here, and like [`Self::PickClipSound`] the
    /// handler moves the dialog onto a thread of its own.
    ChangeClipsFolder,
    /// The settings page asked for the "choose a clip sound" dialog. Handled
    /// on a thread of its own rather than on the worker, so the clip hotkey
    /// keeps working while the dialog is open.
    PickClipSound,
    Quit,
    /// The pump finished a rebind. Carries the outcome so the settings page
    /// can say "that combination is taken" instead of going quiet, and the
    /// kind so it can say *which row* — with two hotkey rows on that page, an
    /// outcome that names only the combination leaves the page guessing by
    /// string comparison.
    HotkeyRebound {
        kind: HotkeyKind,
        spec: String,
        registered: bool,
    },
}

/// Which combination a registration or a rebind refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyKind {
    Clip,
    Screenshot,
}

impl HotkeyKind {
    pub(crate) fn id(self) -> i32 {
        match self {
            Self::Clip => HOTKEY_ID,
            Self::Screenshot => SHOT_HOTKEY_ID,
        }
    }

    /// Which kind an arriving press is for, or `None` for an id this window
    /// never registered.
    ///
    /// The inverse of [`Self::id`], and the reason both hotkey mechanisms can
    /// share one arm in `wnd_proc`: the system hotkey table and
    /// [`crate::hook`] both name the key by the same id.
    fn from_id(id: i32) -> Option<Self> {
        match id {
            HOTKEY_ID => Some(Self::Clip),
            SHOT_HOTKEY_ID => Some(Self::Screenshot),
            _ => None,
        }
    }

    /// The config key this kind is stored under, for log lines that would
    /// otherwise have to pretend every hotkey is the clip one.
    fn config_key(self) -> &'static str {
        match self {
            Self::Clip => "clip_hotkey",
            Self::Screenshot => "screenshot_hotkey",
        }
    }
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
/// Asks the pump to re-register whichever hotkeys are pending in
/// [`PENDING_HOTKEY`] — one message can cover a rebind of either key, or both.
pub(crate) const WM_TRIX_REHOTKEY: u32 = WM_APP + 0x12;
/// Asks the pump for the "choose a clip sound" dialog.
pub(crate) const WM_TRIX_PICK_SOUND: u32 = WM_APP + 0x13;
/// Asks the pump for the "change clips folder" dialog — the same one the tray
/// menu opens, asked for by the settings page instead.
pub(crate) const WM_TRIX_PICK_FOLDER: u32 = WM_APP + 0x14;
/// A hotkey the low-level hook saw, `wparam` carrying the same
/// [`HotkeyKind::id`] the system hotkey table would have put in `WM_HOTKEY`.
///
/// Posted by [`crate::hook`]'s callback so the press leaves that callback
/// immediately — it runs on the pump thread against a 300 ms budget, after
/// which Windows removes the hook silently. A message of Trix's own rather
/// than a synthesised `WM_HOTKEY`, so a reader can tell from the message
/// alone which mechanism heard the key.
pub(crate) const WM_TRIX_HOOK_HOTKEY: u32 = WM_APP + 0x15;
/// The hotkey id. Process-unique is enough — the window owns the only one.
const HOTKEY_ID: i32 = 1;
/// The screenshot hotkey's id. Process-unique is enough — the window owns
/// both of them.
const SHOT_HOTKEY_ID: i32 = 2;

/// The spec a pending rebind wants, one named field per [`HotkeyKind`], left
/// here because `PostMessageW` carries two integers and neither a
/// `HotkeyKind` nor a `String` is one of them.
///
/// A named pair rather than a `[_; 2]` indexed by an ad hoc `slot()`: the
/// fields are addressed by an exhaustive `match` in [`PendingHotkeys::get_mut`]
/// instead of a computed index, so there is no panicking index here to be the
/// one exception to this workspace's `panic = "abort"` rule against them.
///
/// One field per kind, not one shared between them. `state.rs`'s `config.set`
/// handler can rebind both keys back to back from a single request, posting
/// `WM_TRIX_REHOTKEY` twice within microseconds of each other while the pump
/// still has to wake from `GetMessageW` to read either one. A single shared
/// field would let the second `set_pending_hotkey` overwrite the first before
/// the pump ever looked — silently dropping that key's rebind, with the old
/// combination staying live and `config.set` having already answered
/// `requires_rearm: []`. Before there were two keys, a shared field could only
/// ever lose a stale spec *for the same key*, where last-write-wins was the
/// correct behaviour; a second, unrelated key sharing the field is what turns
/// the same overwrite into a bug.
#[derive(Default)]
struct PendingHotkeys {
    clip: Option<String>,
    screenshot: Option<String>,
}

impl PendingHotkeys {
    fn get_mut(&mut self, kind: HotkeyKind) -> &mut Option<String> {
        match kind {
            HotkeyKind::Clip => &mut self.clip,
            HotkeyKind::Screenshot => &mut self.screenshot,
        }
    }
}

static PENDING_HOTKEY: Mutex<PendingHotkeys> =
    Mutex::new(PendingHotkeys { clip: None, screenshot: None });

fn set_pending_hotkey(kind: HotkeyKind, spec: &str) {
    if let Ok(mut pending) = PENDING_HOTKEY.lock() {
        *pending.get_mut(kind) = Some(spec.to_string());
    }
}

/// Drains every pending rebind, not just one. `WM_TRIX_REHOTKEY` is a "go look
/// at `PENDING_HOTKEY`" nudge rather than a message that names which key
/// changed, so one delivery has to be able to answer for two rebinds posted
/// before the pump got around to reading either — see [`PENDING_HOTKEY`]'s doc
/// comment for why leaving one behind is exactly the bug this replaced.
fn take_pending_hotkeys() -> Vec<(HotkeyKind, String)> {
    let Ok(mut pending) = PENDING_HOTKEY.lock() else {
        return Vec::new();
    };
    [HotkeyKind::Clip, HotkeyKind::Screenshot]
        .into_iter()
        .filter_map(|kind| pending.get_mut(kind).take().map(|spec| (kind, spec)))
        .collect()
}

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
    /// So `wnd_proc`'s `WM_HOTKEY` arm can broadcast `hotkey_pressed` directly
    /// instead of only offering `Action::Clip` to the worker — see that arm's
    /// comment for why a full action queue makes the difference matter.
    static CLIENTS: std::cell::RefCell<Option<Arc<Clients>>> = const { std::cell::RefCell::new(None) };
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

/// Asks the pump to re-register a hotkey. Returns immediately; the
/// outcome arrives as an `Action::HotkeyRebound`.
///
/// A no-op when there is no pump — unit tests and the window of shutdown after
/// the pump has gone. A hotkey that cannot be rebound because nothing is
/// listening is not an error worth propagating into `config.set`, which has
/// already saved the value the next startup will register.
pub fn rebind_hotkey(kind: HotkeyKind, spec: &str) {
    // `WINDOW_HWND` is process-global (see its doc comment above), not
    // per-`Daemon`. `cargo test` runs every test in this crate's test binary
    // as concurrent threads, and `a_hotkey_that_cannot_be_registered_still_
    // leaves_a_live_window` below keeps a real pump alive with a real,
    // non-zero `HWND` in that same static for the length of its run. Without
    // this guard, a `dispatch.rs` test calling `config.set` with a
    // *parseable* `clip_hotkey` could read that other test's live HWND here,
    // post `WM_TRIX_REHOTKEY` to it, and have the pump call the real
    // `RegisterHotKey` — taking a system-wide hotkey combination on the
    // developer's own desktop. So this returns before reading `WINDOW_HWND`
    // or writing `PENDING_HOTKEY` at all, which makes that impossible rather
    // than merely unlikely (a mutex serializing the two tests would still
    // leave the hazard one careless future test away).
    //
    // Scope, precisely: `cfg!(test)` is true only in this crate's own unit-test
    // binary, which is where the adversarial pair lives. The files under
    // `tests/` are separate crates linking the *non-test* build of this lib, so
    // the guard is simply absent there and `window::rebind_hotkey` is fully
    // importable. Nothing in `tests/` calls it or `window::spawn` today, and
    // `WINDOW_HWND` stays 0 in a binary that never starts a pump — but an
    // integration test that starts one gets no protection from this line.
    if cfg!(test) {
        return;
    }
    let hwnd = WINDOW_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    set_pending_hotkey(kind, spec);
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            WM_TRIX_REHOTKEY,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// Switches which mechanism the hotkeys are heard through, and re-binds both.
///
/// Both, never one: `hotkey_mode` is a property of the mechanism, so changing
/// it invalidates every live registration at once. Leaving the screenshot key
/// on the old mechanism would produce exactly the half-configured state the
/// single config key exists to prevent.
///
/// The combinations come from the caller rather than being remembered here.
/// `state.rs` owns the config and has just written it; a second copy of "what
/// is the clip key" living in this module is the kind of thing that goes
/// stale.
///
/// Takes effect live, like a rebind of either combination. With no pump
/// (unit tests, the tail of shutdown) the store below is inert — the rebinds
/// return before touching anything, and nothing reads it again until the next
/// startup sets it from the config file.
pub fn set_hotkey_mode(mode: HotkeyMode, clip_spec: &str, screenshot_spec: &str) {
    LOW_LEVEL_HOTKEYS.store(mode == HotkeyMode::LowLevel, Ordering::Release);
    rebind_hotkey(HotkeyKind::Clip, clip_spec);
    rebind_hotkey(HotkeyKind::Screenshot, screenshot_spec);
}

/// Binds one combination through whichever mechanism [`LOW_LEVEL_HOTKEYS`]
/// currently names, after releasing whatever that kind held before. Returns
/// whether the key came up live.
///
/// Both mechanisms are released every time, unconditionally, rather than only
/// the one about to be used: a mode change arrives here as an ordinary rebind,
/// so the registration being replaced is routinely the *other* kind of
/// registration. `UnregisterHotKey` on an id nobody holds simply fails,
/// harmlessly, and `hook::unbind` on a kind that was never bound is a no-op —
/// which is what lets this ask no questions about how the last binding was
/// made.
///
/// A hotkey that will not bind is a **warning, never a failure**. The daemon
/// stays fully usable from the tray and over the socket, and refusing to start
/// because another program owns Alt+F10 would be a far worse product than one
/// dead key.
///
/// # Panics
/// Never, but it must run on the pump thread: both mechanisms bind to the
/// calling thread, and `hook` keeps its state in thread-locals for that
/// reason.
fn bind_hotkey(hwnd: HWND, kind: HotkeyKind, spec: &str) -> bool {
    let key = kind.config_key();
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), kind.id());
    }
    hook::unbind(kind);

    let hotkey = match Hotkey::parse(spec) {
        Ok(hotkey) => hotkey,
        Err(e) => {
            tracing::warn!(spec = %spec, key, error = %format!("{e:#}"), "hotkey is not parseable; nothing registered");
            hotkey_state(kind).store(HOTKEY_TAKEN, Ordering::Release);
            return false;
        }
    };
    let low_level = LOW_LEVEL_HOTKEYS.load(Ordering::Acquire);
    let bound = if low_level {
        hook::bind(hwnd, kind, &hotkey)
    } else {
        unsafe { hotkey.register(hwnd, kind.id()) }
    };
    let live = match bound {
        Ok(()) => {
            tracing::info!(hotkey = %hotkey, key, low_level, "hotkey registered");
            true
        }
        Err(e) => {
            tracing::warn!(
                hotkey = %hotkey,
                key,
                low_level,
                error = %format!("{e:#}"),
                "hotkey unavailable — clip from the tray, rebind it, or try hotkey_mode = \"low_level\""
            );
            false
        }
    };
    // Recorded, not only logged. This is the single line that turns a silently
    // dead key into one `status` can explain — see [`hotkey_bound`].
    hotkey_state(kind).store(if live { HOTKEY_BOUND } else { HOTKEY_TAKEN }, Ordering::Release);
    live
}

/// Asks the pump for the "choose a clip sound" dialog. Returns immediately.
///
/// A no-op when there is no pump — unit tests, and `trix.exe`. The `cfg!(test)`
/// guard is the same one `rebind_hotkey` carries and for the same reason:
/// `WINDOW_HWND` is process-global, and a test in this crate could otherwise
/// read a live pump another test is running and open a real file dialog on the
/// developer's desktop.
/// Asks the pump for the "change clips folder" dialog. Returns immediately.
///
/// The settings page's route to the dialog the tray menu already had. Same
/// no-op-under-test guard as [`request_sound_pick`] below, and for the same
/// reason: `WINDOW_HWND` is process-global, so a test could otherwise open a
/// real folder browser on the developer's desktop.
pub fn request_folder_pick() {
    if cfg!(test) {
        return;
    }
    let hwnd = WINDOW_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            WM_TRIX_PICK_FOLDER,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

pub fn request_sound_pick() {
    if cfg!(test) {
        return;
    }
    let hwnd = WINDOW_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            WM_TRIX_PICK_SOUND,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// What a hotkey press does, once one has arrived.
///
/// Shared by both mechanisms — see the `WM_HOTKEY | WM_TRIX_HOOK_HOTKEY` arm.
/// Whatever heard the key, this is the only description of what a press means,
/// so the two modes cannot drift into behaving differently.
fn fire_hotkey(kind: HotkeyKind) {
    match kind {
        HotkeyKind::Clip => {
            // Broadcast before `offer`, and unconditionally: `offer` drops
            // `Action::Clip` when the worker's queue is already full (a user
            // mashing the hotkey during a mux), and `handle_action` — where
            // this used to live — never runs for a dropped action. That made
            // the daemon receive a press and report nothing, which is exactly
            // the false negative the settings page's "press it now" test
            // (spec §6.4) exists to rule out: it asks whether the *daemon*
            // received the combination, not whether a clip resulted, because
            // an overlay that has silently stolen the hotkey looks identical
            // to a slow clip from here otherwise. Broadcasting from the
            // message itself, before the queue can drop anything, means a
            // dropped clip no longer also costs the event.
            CLIENTS.with(|c| {
                if let Some(clients) = c.borrow().as_ref() {
                    clients.broadcast(&Event::new("hotkey_pressed", Value::Object(Map::new())));
                }
            });
        }
        // No `hotkey_pressed` broadcast for a screenshot, deliberately. The
        // settings page listens for that event to confirm the *clip*
        // combination reached the daemon (spec §6.4); firing it for a
        // screenshot would make that test pass for the wrong key, which is
        // worse than it not passing at all.
        HotkeyKind::Screenshot => {}
    }
    ACTIONS.with(|a| {
        if let Some(tx) = a.borrow().as_ref() {
            offer(
                tx,
                match kind {
                    HotkeyKind::Clip => Action::Clip,
                    HotkeyKind::Screenshot => Action::Screenshot,
                },
            );
        }
    });
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
        // One arm for both mechanisms. `WM_HOTKEY` comes from the system
        // hotkey table, `WM_TRIX_HOOK_HOTKEY` from the low-level keyboard
        // hook, and past this point a press is a press: whichever way the
        // user's `hotkey_mode` heard the key, what it does next must be
        // identical, and two arms is how that stops being true.
        //
        // An id neither mechanism registered is not ours; `LRESULT(0)` is
        // what `DefWindowProcW` would have answered for a stray `WM_HOTKEY`
        // anyway.
        WM_HOTKEY | WM_TRIX_HOOK_HOTKEY => {
            if let Some(kind) = HotkeyKind::from_id(wparam.0 as i32) {
                fire_hotkey(kind);
            }
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
        WM_TRIX_REHOTKEY => {
            // Every pending rebind, not just one — see `take_pending_hotkeys`'s
            // doc comment for why a single `WM_TRIX_REHOTKEY` delivery has to
            // be able to answer for both keys.
            for (kind, spec) in take_pending_hotkeys() {
                // The old binding is released inside `bind_hotkey`, first and
                // unconditionally: leaving it alive would mean two live
                // hotkeys, with the one the user just replaced still bound to
                // the old combination. The same call is what the startup
                // registration makes, so a rebind and a fresh start cannot
                // disagree about which mechanism a mode means.
                let registered = bind_hotkey(hwnd, kind, &spec);
                ACTIONS.with(|a| {
                    if let Some(tx) = a.borrow().as_ref() {
                        offer(tx, Action::HotkeyRebound { kind, spec, registered });
                    }
                });
            }
            LRESULT(0)
        }
        WM_TRIX_PICK_FOLDER => {
            // Forwarding only, exactly like the sound arm below: the dialog
            // must not run on the pump, or the tray icon stops answering for
            // as long as the user browses.
            ACTIONS.with(|a| {
                if let Some(tx) = a.borrow().as_ref() {
                    offer(tx, Action::ChangeClipsFolder);
                }
            });
            LRESULT(0)
        }
        WM_TRIX_PICK_SOUND => {
            // The pump only forwards. Running a modal dialog here would stop
            // the tray icon answering for as long as the user browses, which
            // is the failure `folder.rs` documents at length.
            ACTIONS.with(|a| {
                if let Some(tx) = a.borrow().as_ref() {
                    offer(tx, Action::PickClipSound);
                }
            });
            LRESULT(0)
        }
        tray::WM_TRIX_TRAY => {
            // Without NOTIFYICON_VERSION_4 (which we do not ask for) the
            // callback's lparam is the mouse message itself.
            let action = match lparam.0 as u32 {
                // Left click opens the UI.
                WM_LBUTTONUP => Some(Action::OpenApp),
                WM_RBUTTONUP => {
                    // The menu is modal and pumps its own messages, but it is
                    // driven by the user and returns promptly, so it is the
                    // one thing this thread is allowed to do inline.
                    let armed = ARMED_MIRROR.load(Ordering::Relaxed);
                    match unsafe { tray::show_menu(hwnd, armed) } {
                        Some(tray::ID_TOGGLE) => Some(Action::ToggleArmed),
                        Some(tray::ID_OPEN_UI) => Some(Action::OpenApp),
                        Some(tray::ID_OPEN_FOLDER) => Some(Action::OpenClipsFolder),
                        Some(tray::ID_CHANGE_FOLDER) => Some(Action::ChangeClipsFolder),
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

/// Starts the pump thread, creates the window, and binds `clip_hotkey` and
/// `screenshot_hotkey` through the mechanism `hotkey_mode` names.
///
/// Returns once the window exists, so a caller can attach a tray icon to it
/// immediately. A hotkey that fails to bind is a **warning, not an error**:
/// another program owning Alt+F10 (NVIDIA's overlay does exactly this) must
/// not stop the daemon from starting — the user can still clip from the tray,
/// can rebind in settings, and can switch `hotkey_mode` to hear the
/// combination the other program took.
///
/// `clients` is what lets the hotkey arm broadcast `hotkey_pressed` straight
/// from the pump, rather than only from `handle_action` on the worker thread —
/// see [`fire_hotkey`] for why the difference matters.
pub fn spawn(
    actions: SyncSender<Action>,
    hotkey_spec: &str,
    shot_hotkey_spec: &str,
    hotkey_mode: HotkeyMode,
    clients: Arc<Clients>,
) -> Result<WindowHandle> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<isize>>();
    // Before the thread starts, so the two `bind_hotkey` calls inside it read
    // the user's choice rather than racing this line to the default.
    LOW_LEVEL_HOTKEYS.store(hotkey_mode == HotkeyMode::LowLevel, Ordering::Release);
    let spec = hotkey_spec.to_string();
    let shot_spec = shot_hotkey_spec.to_string();
    let finished = Arc::new(AtomicBool::new(false));
    let pump_finished = Arc::clone(&finished);

    let join = std::thread::Builder::new()
        .name("trix-window".into())
        .spawn(move || {
            ACTIONS.with(|a| *a.borrow_mut() = Some(actions));
            CLIENTS.with(|c| *c.borrow_mut() = Some(clients));
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

            bind_hotkey(hwnd, HotkeyKind::Clip, &spec);
            bind_hotkey(hwnd, HotkeyKind::Screenshot, &shot_spec);

            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            // Both unregistered unconditionally, and the errors ignored — same
            // precedent as the `WM_TRIX_REHOTKEY` arm above. Whether either
            // hotkey is actually held at this point depends on the startup
            // result *and* on any `WM_TRIX_REHOTKEY` that ran since, and there
            // is no cheap way to ask the OS "is this id currently mine" short
            // of just releasing it: `UnregisterHotKey` on an id that was never
            // registered simply fails, harmlessly. Windows would reclaim a
            // leaked registration at thread exit anyway, but leaving that to
            // chance would mean these lines no longer mean what they say.
            unsafe {
                let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                let _ = UnregisterHotKey(Some(hwnd), SHOT_HOTKEY_ID);
            }
            // And the other mechanism, on the same terms: at most one of the
            // two is actually holding anything, this is the thread that
            // installed the hook if there is one, and a hook left behind is
            // the kind that outlives its reason to exist.
            hook::uninstall();
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
        Action::Clip => {
            // `hotkey_pressed` is broadcast from `wnd_proc`'s `WM_HOTKEY` arm
            // itself now, not here — see that arm's comment. This action can
            // be dropped by a full queue (`offer` in `window::wnd_proc`)
            // before it ever reaches this match, and the event has to survive
            // that; broadcasting only on this side once did not.
            //
            // `clip_saved` is not emitted here either: `Daemon::clip` emits it
            // itself, so this path and the socket's `clip` command announce a
            // saved clip identically. They did not always -- only the socket
            // command did, which meant a hotkey clip reached disk and no
            // client was ever told.
            match daemon.clip() {
                Ok(Some(meta)) => {
                    tracing::info!(clip = %meta.id, "clip saved from the hotkey")
                }
                // `arm` only starts filling the ring when the first frame
                // arrives, so a hotkey pressed in the first moments after
                // arming is a real "nothing buffered yet" rather than a
                // failure.
                Ok(None) => tracing::warn!("nothing buffered yet — no clip saved"),
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "clip failed"),
            }
        }
        Action::Screenshot => {
            // `shot_saved` is not emitted here: `Daemon::screenshot` emits it
            // itself (from `record_saved_shot`), the same way `Daemon::clip`
            // emits `clip_saved` above -- so this path and the socket's
            // `screenshot` command announce a saved shot identically, and a
            // hotkey screenshot is never broadcast twice.
            //
            // A failure is different from `Action::Clip`'s on purpose: Trix
            // does not arm by default, so "press the new hotkey" is the
            // *ordinary* first experience of this feature, not a corner case,
            // and the design this branch was built from is explicit that
            // disarmed silence is the one failure this feature may not have.
            // The socket `screenshot` command already reaches a client
            // through `dispatch.rs`'s `fail` helper, but nothing in the
            // desktop app sends that command -- the hotkey is the only path
            // that exists in the shipped product, so it has to raise the
            // toast itself rather than leaning on a dispatch arm it never
            // goes through. Built by hand rather than reusing `dispatch.rs`'s
            // private `error_data` (out of reach here) or `serde_json::json!`
            // (hides an `unwrap`), but the shape matches it exactly:
            // `{"cmd":"screenshot","error":…}`, which `state.svelte.ts`'s
            // `case 'error'` already renders as a toast with no client-side
            // change needed.
            match daemon.screenshot() {
                Ok(meta) => {
                    tracing::info!(shot = %meta.id, "screenshot saved from the hotkey")
                }
                Err(e) => {
                    let error = format!("{e:#}");
                    tracing::warn!(error = %error, "screenshot failed");
                    let mut fields = Map::new();
                    fields.insert("cmd".to_string(), Value::from("screenshot"));
                    fields.insert("error".to_string(), Value::from(error));
                    daemon.clients.broadcast(&Event::new("error", Value::Object(fields)));
                }
            }
        }
        Action::HotkeyRebound { kind, spec, registered } => {
            // `key` names the config key rather than the kind's Rust name, so
            // a client can match it against the field it just saved without
            // knowing anything about this enum.
            daemon.clients.broadcast(&Event::new(
                "hotkey_rebound",
                serde_json::json!({
                    "key": kind.config_key(),
                    "spec": spec,
                    "registered": registered,
                }),
            ));
        }
        // The mirror decides what the menu offered, so a toggle does what the
        // user just read, not what the daemon became a moment later.
        Action::ToggleArmed => set_armed(daemon, !ARMED_MIRROR.load(Ordering::Relaxed)),
        Action::Arm => set_armed(daemon, true),
        Action::Disarm => set_armed(daemon, false),
        Action::OpenApp => open_app(daemon),
        Action::OpenClipsFolder => open_clips_folder(daemon),
        Action::ChangeClipsFolder => change_clips_folder(daemon),
        Action::PickClipSound => choose_clip_sound(daemon),
        Action::Quit => {
            // The same path Ctrl+C takes, so a tray Quit finalizes an
            // in-flight mux exactly like a console close does rather than
            // dropping the clip the user just asked for.
            tracing::info!("quit requested from the tray");
            trix_core::control::request_shutdown();
        }
    }
}

/// Opens the clips folder in Explorer.
fn open_clips_folder(daemon: &Arc<Daemon>) {
    let dir = daemon.clip_dir();
    // `explorer.exe` returns a non-zero exit code even on success, so its
    // status is deliberately ignored rather than logged as a failure the user
    // would see in the log for no reason.
    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    tracing::debug!(dir = %dir.display(), "opened the clips folder");
}

/// Where `trix-ui.exe` lives, given the daemon's own path: beside it, the way
/// spec §8's MSI installs all three binaries and the way cargo builds them.
///
/// Returns `None` when `daemon_exe` has no real parent directory — either
/// because `Path::parent` returns `None` outright (a root or a prefix), or
/// because it returns `Some("")`, which is what a bare relative filename like
/// `trix-daemon.exe` produces. An empty parent still joins into a path
/// (`"" .join("trix-ui.exe")` is `"trix-ui.exe"`), but that bare relative path
/// is exactly what must not reach `Command::spawn`: it would be resolved
/// against the daemon's *current working directory*, which for a process
/// started from the autostart registry entry, a smoke script, or an arbitrary
/// shell is not where the binaries actually live. Treating an empty parent
/// the same as no parent is what makes the caller fall back to opening the
/// clips folder instead of launching whatever happens to sit in the cwd.
fn ui_path_beside(daemon_exe: &Path) -> Option<PathBuf> {
    let parent = daemon_exe.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some(parent.join("trix-ui.exe"))
}

/// Opens the desktop app, or the clips folder if it is not installed.
///
/// The fallback is not politeness: `trix-daemon.exe` is a supported thing to
/// run on its own (it is what the autostart entry runs, and what the smoke
/// scripts drive), so "Open Trix" has to do something useful on a machine that
/// has the daemon and no app. The app's own single-instance plugin handles the
/// second click — this side always just runs the exe.
fn open_app(daemon: &Arc<Daemon>) {
    let ui = std::env::current_exe().ok().and_then(|exe| ui_path_beside(&exe));
    let Some(ui) = ui.filter(|path| path.exists()) else {
        open_clips_folder(daemon);
        return;
    };
    if let Err(error) = std::process::Command::new(&ui).spawn() {
        tracing::warn!(path = %ui.display(), %error, "could not start the Trix app");
        open_clips_folder(daemon);
    }
}

/// "Change clips folder…", end to end — the tray menu's item and the settings
/// page's `folder.pick`, which are the same dialog and the same handler.
///
/// **On a detached thread, like [`choose_clip_sound`] below.** The worker
/// thread also handles [`Action::Clip`], so a modal dialog held open on it
/// means the clip hotkey does nothing until the user finishes browsing. That
/// was survivable while this lived only in a tray menu; it is not once the
/// settings page has a button for it, and the fix was always the sound
/// dialog's.
///
/// The guard makes a second click while a dialog is open a no-op rather than a
/// second dialog — and there are now two places to click.
///
/// The chosen path goes through [`Daemon::set_config`] rather than being
/// applied here, and that is the point of the whole feature: both callers
/// inherit the control protocol's validation, its all-or-nothing write to
/// `config.toml`, and its live in-memory apply. A second path that wrote the
/// setting itself would be a second set of rules to keep true, and the one that
/// drifted would be this one — it has no tests, because it is a dialog.
fn change_clips_folder(daemon: &Arc<Daemon>) {
    static PICKING: AtomicBool = AtomicBool::new(false);
    if PICKING.swap(true, Ordering::SeqCst) {
        return;
    }

    let daemon = Arc::clone(daemon);
    let spawned = std::thread::Builder::new().name("trix-folder-dialog".into()).spawn(move || {
        // Read inside the thread rather than before the spawn: this same value
        // is what the refusal message quotes, and it must be the folder clips
        // are *still* going to, not one captured before the user browsed.
        let current = daemon.clip_dir();

        let chosen = crate::folder::pick(&current);
        PICKING.store(false, Ordering::SeqCst);

        let chosen = match chosen {
            Ok(Some(path)) => path,
            // Cancelled. Not a failure, and deliberately silent: answering a
            // dialog the user dismissed on purpose with a message box is the
            // behaviour that makes people stop opening menus.
            Ok(None) => return,
            Err(e) => {
                let detail = format!("{e:#}");
                tracing::warn!(error = %detail, "the folder picker failed");
                crate::folder::report_error(&format!(
                    "Could not open the folder picker.

{detail}"
                ));
                return;
            }
        };

        let mut values = serde_json::Map::new();
        values.insert(
            "clip_dir".to_string(),
            serde_json::Value::from(chosen.to_string_lossy().as_ref()),
        );
        // Through the dispatch helper, not `set_config` directly: an
        // already-open settings page only learns a folder changed elsewhere
        // via the `config_changed` broadcast that helper sends -- see its doc
        // comment.
        match crate::dispatch::apply_config_and_broadcast(&daemon, &values) {
            // No re-arm: `clip_dir` is read per clip, so a running capture
            // keeps its ring and the very next clip lands in the new folder.
            Ok(_) => tracing::info!(dir = %chosen.display(), "clips folder changed"),
            Err(detail) => {
                tracing::warn!(
                    error = %detail,
                    dir = %chosen.display(),
                    "the chosen clips folder was refused"
                );
                // Says where clips are still going, not just what failed. A
                // user told only "that didn't work" does not know whether they
                // are now recording to nowhere.
                crate::folder::report_error(&format!(
                    "That folder can't be used:

{detail}

Clips are still being saved to:
{}",
                    current.display()
                ));
            }
        }
    });

    if spawned.is_err() {
        PICKING.store(false, Ordering::SeqCst);
        tracing::warn!("could not start the folder-dialog thread");
    }
}

/// Runs the sound dialog and applies the result.
///
/// **On a detached thread, unlike `change_clips_folder`.** The worker thread
/// also handles `Action::Clip`, so a modal dialog held open on it means the
/// clip hotkey does nothing until the user finishes browsing. The folder picker
/// has always had that flaw; there is no reason to copy it. `Arc<Daemon>` is
/// already in hand here, so the thread costs nothing but a clone.
///
/// The guard makes a second click while a dialog is open a no-op rather than a
/// second dialog.
fn choose_clip_sound(daemon: &Arc<Daemon>) {
    static PICKING: AtomicBool = AtomicBool::new(false);
    if PICKING.swap(true, Ordering::SeqCst) {
        return;
    }

    let daemon = Arc::clone(daemon);
    let spawned = std::thread::Builder::new().name("trix-sound-dialog".into()).spawn(move || {
        let chosen = crate::folder::pick_sound();
        PICKING.store(false, Ordering::SeqCst);

        let chosen = match chosen {
            Ok(Some(path)) => path,
            // Cancelled, and deliberately silent: answering a dialog the user
            // dismissed on purpose with a message box is what makes people stop
            // opening menus.
            Ok(None) => return,
            Err(e) => {
                let detail = format!("{e:#}");
                tracing::warn!(error = %detail, "the sound picker failed");
                crate::folder::report_error(&format!(
                    "Could not open the sound picker.\n\n{detail}"
                ));
                return;
            }
        };

        let mut values = serde_json::Map::new();
        values.insert(
            "clip_sound_path".to_string(),
            serde_json::Value::from(chosen.to_string_lossy().as_ref()),
        );
        // Through the dispatch helper, not `set_config` directly: the settings
        // page is open in another process and learns about this only from the
        // `config_changed` broadcast that helper sends.
        match crate::dispatch::apply_config_and_broadcast(&daemon, &values) {
            // `_` because the helper returns the `ConfigUpdate`; nothing here
            // needs it, and `Ok(())` would not typecheck.
            Ok(_) => tracing::info!(sound = %chosen.display(), "clip sound changed"),
            Err(e) => {
                crate::folder::report_error(&format!("That sound can't be used:\n\n{e}"));
            }
        }
    });

    if spawned.is_err() {
        PICKING.store(false, Ordering::SeqCst);
        tracing::warn!("could not start the sound-dialog thread");
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

    /// Both the menu item and a left click open the app now. Until this task
    /// they opened the clips folder, which was the right placeholder for a
    /// daemon with no app and is the wrong behaviour for one that has it.
    #[test]
    fn opening_trix_launches_the_app_beside_the_daemon() {
        let daemon = Path::new(r"C:\Program Files\Trix\trix-daemon.exe");
        assert_eq!(
            ui_path_beside(daemon),
            Some(PathBuf::from(r"C:\Program Files\Trix\trix-ui.exe"))
        );
        // A bare filename has no real parent directory (`Path::parent`
        // returns `Some("")`, not `None`), so this must fall back rather than
        // hand back a bare `trix-ui.exe` for `Command::spawn` to resolve
        // against the daemon's current working directory.
        assert_eq!(ui_path_beside(Path::new("trix-daemon.exe")), None);
    }

    /// A hotkey the user changes in settings has to work now, not after a
    /// restart they were never told to perform. The pump owns the
    /// registration (`RegisterHotKey` posts to the *registering thread's*
    /// queue), so a rebind is a message to the pump plus the new spec left
    /// where the pump can read it.
    ///
    /// Sets both kinds, the way `state.rs`'s `config.set` handler does when a
    /// single request changes `clip_hotkey` and `screenshot_hotkey` together,
    /// and asserts both come back out. A shared one-slot pending value used to
    /// let the second `set_pending_hotkey` silently overwrite the first —
    /// see `PENDING_HOTKEY`'s doc comment — which this would have caught by
    /// getting only one entry back instead of two.
    #[test]
    fn a_rebind_leaves_the_new_spec_for_the_pump() {
        set_pending_hotkey(HotkeyKind::Clip, "ctrl+shift+f10");
        set_pending_hotkey(HotkeyKind::Screenshot, "ctrl+shift+f9");
        assert_eq!(
            take_pending_hotkeys(),
            vec![
                (HotkeyKind::Clip, "ctrl+shift+f10".to_string()),
                (HotkeyKind::Screenshot, "ctrl+shift+f9".to_string()),
            ]
        );
        assert_eq!(
            take_pending_hotkeys(),
            Vec::new(),
            "the pump takes every pending request once; a second WM_TRIX_REHOTKEY must not re-register a stale spec"
        );
    }

    /// `wnd_proc`'s `WM_HOTKEY` arm broadcasts before it ever calls `offer`,
    /// so a press the daemon received still reaches a listening client even
    /// when the worker's action queue is already full and `Action::Clip` gets
    /// dropped -- the false negative this change exists to close (see that
    /// arm's comment, and `Action::Clip`'s in `handle_action`).
    ///
    /// Posts `WM_HOTKEY` directly rather than registering a real global
    /// hotkey and pressing it: `wnd_proc` only checks `wparam == HOTKEY_ID`,
    /// and posting it by hand from another thread is the same technique
    /// `WindowHandle::request_exit` and `rebind_hotkey` already use to reach
    /// this pump.
    #[test]
    fn hotkey_pressed_reaches_a_client_even_when_the_action_queue_is_full() {
        let (tx, rx) = sync_channel::<Action>(ACTION_QUEUE_DEPTH);
        // Filled before the window exists, so the `offer` inside `wnd_proc`'s
        // `WM_HOTKEY` arm below is guaranteed to find the queue full and drop
        // what it sends -- the exact condition this test is about.
        for _ in 0..ACTION_QUEUE_DEPTH {
            tx.try_send(Action::Clip).expect("queue accepts up to its depth");
        }

        let clients = Arc::new(Clients::default());
        let (out_tx, out_rx) = sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        clients.register(out_tx);

        let mut window =
            spawn(tx, "not+a+hotkey", "not+a+hotkey", HotkeyMode::Standard, Arc::clone(&clients))
                .expect("an unusable hotkey must not fail the daemon");

        unsafe {
            let _ =
                PostMessageW(Some(window.hwnd()), WM_HOTKEY, WPARAM(HOTKEY_ID as usize), LPARAM(0));
        }

        let line = out_rx.recv_timeout(std::time::Duration::from_secs(2)).expect(
            "hotkey_pressed must reach a registered client even though the action queue was full",
        );
        let event: Event = serde_json::from_str(line.trim_end())
            .unwrap_or_else(|e| panic!("event line did not decode: {e}\nline: {line}"));
        assert_eq!(event.event, "hotkey_pressed");

        drop(rx); // never drained on purpose; see the comment above
        window.shutdown();
    }

    /// The screenshot hotkey's silence is a deliberate design decision (see
    /// the `WM_HOTKEY` arm for `SHOT_HOTKEY_ID` above, and `Action::Screenshot`'s
    /// match arm in `handle_action`): the settings page's "press it now" test
    /// relies on `hotkey_pressed` meaning "the *clip* combination reached the
    /// daemon", and broadcasting it for the screenshot key too would make that
    /// test pass for the wrong hotkey. Nothing enforced that until this test —
    /// a future refactor unifying the two `WM_HOTKEY` arms could start
    /// broadcasting `hotkey_pressed` for both and nothing would fail.
    ///
    /// Same technique as `hotkey_pressed_reaches_a_client_even_when_the_
    /// action_queue_is_full` above: post `WM_HOTKEY` by hand rather than
    /// register a real global hotkey, since `wnd_proc` only ever inspects
    /// `wparam`.
    #[test]
    fn the_screenshot_hotkey_reaches_the_worker_but_never_broadcasts_hotkey_pressed() {
        let (tx, rx) = sync_channel::<Action>(ACTION_QUEUE_DEPTH);

        let clients = Arc::new(Clients::default());
        let (out_tx, out_rx) = sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        clients.register(out_tx);

        let mut window =
            spawn(tx, "not+a+hotkey", "not+a+hotkey", HotkeyMode::Standard, Arc::clone(&clients))
                .expect("an unusable hotkey must not fail the daemon");

        unsafe {
            let _ = PostMessageW(
                Some(window.hwnd()),
                WM_HOTKEY,
                WPARAM(SHOT_HOTKEY_ID as usize),
                LPARAM(0),
            );
        }

        let action = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the screenshot hotkey must still reach the worker");
        assert_eq!(action, Action::Screenshot);

        match out_rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            other => {
                panic!("the screenshot hotkey must never broadcast hotkey_pressed, got {other:?}")
            }
        }

        window.shutdown();
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
        let mut window = spawn(
            tx,
            "not+a+hotkey",
            "not+a+hotkey",
            HotkeyMode::Standard,
            Arc::new(Clients::default()),
        )
        .expect("an unusable hotkey must not fail the daemon");
        assert!(!window.hwnd().0.is_null(), "the window must exist even with no hotkey");

        // And the failure is *recorded*, not only logged. This is the whole of
        // the fix for a key that used to die silently: before it, the daemon's
        // entire response to a combination it could not have was one
        // `tracing::warn!` into a log with no file behind it, so `status` had
        // nothing to say and the settings page had nothing to show.
        //
        // The pump binds on its own thread after `spawn` has already returned,
        // so this polls rather than asserting once -- an immediate read would
        // race the binding and see the honest "not tried yet" instead.
        let mut recorded = None;
        for _ in 0..100 {
            recorded = hotkey_bound(HotkeyKind::Clip);
            if recorded.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            recorded,
            Some(false),
            "an unparseable hotkey must be reported as not listening, not as untried"
        );

        // Must return rather than hang: the pump is a live thread, and the
        // only thing that ends it is this message.
        window.shutdown();
    }

    /// The direction that matters. Anything that is not an explicit outcome is
    /// "nobody has tried", and that must not decay into "it failed": every
    /// client sees the untried state for the moment between the socket
    /// answering and the pump binding, and a warning that fires at every
    /// launch is one nobody believes on the launch it is right about.
    #[test]
    fn an_untried_hotkey_is_unknown_rather_than_failed() {
        assert_eq!(decode_hotkey_state(HOTKEY_UNTRIED), None);
        assert_eq!(decode_hotkey_state(HOTKEY_BOUND), Some(true));
        assert_eq!(decode_hotkey_state(HOTKEY_TAKEN), Some(false));
        assert_eq!(decode_hotkey_state(200), None, "a value nobody wrote is not a failure either");
    }

    /// Pins the wire shape Task 9's settings page binds to: `hotkey_rebound`
    /// carries exactly `{"spec": "...", "registered": true|false}`.
    ///
    /// `handle_action` never touches `WINDOW_HWND` or `PENDING_HOTKEY` — it
    /// only broadcasts — so this drives it directly against a scratch
    /// `Daemon` rather than through a live pump. A live pump is exactly what
    /// the guard in `rebind_hotkey` above exists to keep this test binary
    /// away from (see that function's doc comment), so reaching one here
    /// would be a step backwards, not a more thorough test.
    ///
    /// The `Daemon` is built the same way `stats.rs`'s `fixture` and
    /// `dispatch.rs`'s `with_scratch_config`/`idle` are: a scratch `clip_dir`
    /// under the temp dir and `config_path: None`, so this neither reads the
    /// developer's real `config.toml` nor scans their real clip library, and
    /// it creates no window.
    #[test]
    fn hotkey_rebound_broadcasts_the_settings_page_wire_shape() {
        use trix_core::config::Config;

        let dir = std::env::temp_dir().join(format!("trix-hotkey-rebound-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp clip dir");
        let config = Config { clip_dir: dir.to_string_lossy().into_owned(), ..Config::default() };
        let daemon = Arc::new(Daemon::new_at(config, None));

        let (tx, rx) = sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        daemon.clients.register(tx);

        handle_action(
            &daemon,
            Action::HotkeyRebound {
                kind: HotkeyKind::Screenshot,
                spec: "ctrl+shift+f9".to_string(),
                registered: true,
            },
        );

        let line = rx.try_recv().expect("HotkeyRebound must broadcast an event");
        let event: Event = serde_json::from_str(line.trim_end())
            .unwrap_or_else(|e| panic!("event line did not decode: {e}\nline: {line}"));
        assert_eq!(event.event, "hotkey_rebound");
        assert_eq!(event.data.get("spec").and_then(Value::as_str), Some("ctrl+shift+f9"));
        assert_eq!(event.data.get("registered").and_then(Value::as_bool), Some(true));
        // The screenshot kind deliberately, not the clip one: with two hotkey
        // rows on the settings page, an event that named only the combination
        // left the page matching strings to guess which row it was about. A
        // `Clip` fixture here would pass whether or not `key` was wired to the
        // kind at all, because `clip_hotkey` is what a wrong answer defaults
        // to.
        assert_eq!(event.data.get("key").and_then(Value::as_str), Some("screenshot_hotkey"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Pins the C1 fix: a screenshot hotkey pressed while disarmed must
    /// broadcast an `error` event, not just a log line. Before this test the
    /// only path that exists in the shipped product for this feature --
    /// nothing in the desktop app ever sends the socket `screenshot` command
    /// -- was silent on failure, which contradicts the spec's explicit
    /// "pressing the key while not armed must say so."
    ///
    /// Same fixture shape as `hotkey_rebound_broadcasts_the_settings_page_
    /// wire_shape` above: a scratch, never-armed `Daemon` driven directly
    /// through `handle_action`, so this touches no real config or clip
    /// library and starts no window.
    #[test]
    fn a_disarmed_screenshot_hotkey_broadcasts_an_error_event() {
        use trix_core::config::Config;

        let dir =
            std::env::temp_dir().join(format!("trix-shot-hotkey-error-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp clip dir");
        let config = Config { clip_dir: dir.to_string_lossy().into_owned(), ..Config::default() };
        let daemon = Arc::new(Daemon::new_at(config, None));

        let (tx, rx) = sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        daemon.clients.register(tx);

        handle_action(&daemon, Action::Screenshot);

        let line = rx.try_recv().expect("a disarmed screenshot must broadcast an `error` event");
        let event: Event = serde_json::from_str(line.trim_end())
            .unwrap_or_else(|e| panic!("event line did not decode: {e}\nline: {line}"));
        assert_eq!(event.event, "error");
        assert_eq!(event.data.get("cmd").and_then(Value::as_str), Some("screenshot"));
        assert_eq!(
            event.data.get("error").and_then(Value::as_str),
            Some("not armed — arm Trix to take a screenshot")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
