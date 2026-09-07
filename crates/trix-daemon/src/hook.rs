//! The low-level keyboard hook: the daemon's second way of hearing its
//! hotkeys, for the case `RegisterHotKey` cannot answer.
//!
//! `RegisterHotKey` *reserves* a combination, and reserving is exactly what
//! makes it fail: when another program already holds Alt+F10 — NVIDIA's
//! overlay does, on a great many machines — the call is refused and Trix never
//! sees that key again. The user's experience is a key that does nothing,
//! forever. `WH_KEYBOARD_LL` reserves nothing. It watches, so a combination
//! somebody else owns is still perfectly visible.
//!
//! **Trix listens; it never intercepts.** [`keyboard_proc`] calls
//! `CallNextHookEx` on every event without exception, including the events it
//! acts on. That is a product decision, not an oversight: this mode exists to
//! be switched on by someone whose hotkey is broken, and a mode that can only
//! ever *add* behaviour is one they can try without wondering what it took
//! away. The cost is that a combination another program owns now fires both —
//! press Alt+F10 with NVIDIA's overlay bound to it and you get their replay
//! and our clip. That is visible and explicable; silently breaking their
//! shortcut would not be.
//!
//! **Why the pump thread.** A low-level hook is installed from a thread that
//! pumps messages and its callback is delivered on that thread. The daemon
//! already has exactly one such thread — the message-only window in
//! [`crate::window`] — so this module adds a hook, not a thread, and inherits
//! that module's standing rule that the pump never blocks. That rule is load
//! bearing here: Windows silently removes a hook whose callback overruns
//! `LowLevelHooksTimeout` (300 ms), and the callback below does a handful of
//! integer comparisons and one `PostMessageW` before returning. Everything
//! slow is already on the far side of that post.

use std::cell::{Cell, RefCell};

use anyhow::{Context as _, Result};
use trix_core::control::{Hotkey, Modifiers};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, PostMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::window::{HotkeyKind, WM_TRIX_HOOK_HOTKEY};

/// One combination the hook is watching for.
struct Binding {
    vk: u32,
    modifiers: Modifiers,
    /// Whether this combination is currently held down.
    ///
    /// `RegisterHotKey` gets this for free from `MOD_NOREPEAT`: hold the key
    /// and it fires once. A hook is handed the OS's key auto-repeat as a
    /// stream of ordinary key-down events, so without this flag a leaned-on
    /// Alt+F10 would queue thirty clips a second.
    down: bool,
}

/// The combinations, one named field per [`HotkeyKind`] rather than an array
/// addressed by a computed index — the same shape, and for the same reason, as
/// `window.rs`'s `PendingHotkeys`: this workspace builds with `panic = "abort"`
/// and an exhaustive `match` has no index to get wrong.
struct Bindings {
    clip: Option<Binding>,
    screenshot: Option<Binding>,
}

impl Bindings {
    const NONE: Self = Self { clip: None, screenshot: None };

    fn get_mut(&mut self, kind: HotkeyKind) -> &mut Option<Binding> {
        match kind {
            HotkeyKind::Clip => &mut self.clip,
            HotkeyKind::Screenshot => &mut self.screenshot,
        }
    }

    fn is_empty(&self) -> bool {
        self.clip.is_none() && self.screenshot.is_none()
    }
}

// Thread-locals, not statics behind a mutex, because every one of these is
// touched on the pump thread and only there: `bind`/`unbind` run inside
// `wnd_proc`, and Windows delivers a low-level hook's callback to the thread
// that installed it. `HHOOK` and `HWND` are not `Send` in the first place, so
// a static would have needed an `isize` and a cast to launder them past the
// compiler — which would have hidden the single-thread invariant rather than
// stated it.
thread_local! {
    /// The installed hook, or `None` while the daemon is in standard mode.
    static HOOK: RefCell<Option<HHOOK>> = const { RefCell::new(None) };
    static BINDINGS: RefCell<Bindings> = const { RefCell::new(Bindings::NONE) };
    /// The window a matched combination is posted to. Zero until `bind`.
    static TARGET: Cell<isize> = const { Cell::new(0) };
}

/// Whether one key event completes a combination.
///
/// Exact, not "at least": `RegisterHotKey` does not fire an Alt+F10 hotkey
/// when Ctrl+Alt+F10 is pressed, and the two modes must not disagree about
/// what the user's own combination means — a hotkey that suddenly answers to
/// more keys than it used to is a worse surprise than one that does not fire.
fn matches(want_vk: u32, want: Modifiers, event_vk: u32, live: Modifiers) -> bool {
    want_vk == event_vk && want == live
}

/// Which modifiers are physically held right now.
///
/// `GetAsyncKeyState` rather than `GetKeyState`: the synchronous call reports
/// the keyboard as of the calling thread's own input queue, and the pump
/// thread has no input of its own — it would answer "nothing is held" while
/// the user holds Alt. The async call reads the physical state, which is the
/// question being asked.
///
/// Either Windows key satisfies `win`, matching `MOD_WIN`.
fn live_modifiers() -> Modifiers {
    fn down(vk: VIRTUAL_KEY) -> bool {
        // The high bit is "down now"; the low bit is "pressed since last
        // asked", which is a different question and not the one this answers.
        (unsafe { GetAsyncKeyState(i32::from(vk.0)) } as u16) & 0x8000 != 0
    }
    Modifiers {
        ctrl: down(VK_CONTROL),
        alt: down(VK_MENU),
        shift: down(VK_SHIFT),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

/// Starts watching for `hotkey` as `kind`, installing the hook if this is the
/// first binding.
///
/// `hwnd` is where a match is posted. It must be the pump's own window, and
/// this must be called on the pump thread.
pub(crate) fn bind(hwnd: HWND, kind: HotkeyKind, hotkey: &Hotkey) -> Result<()> {
    install()?;
    TARGET.with(|target| target.set(hwnd.0 as isize));
    BINDINGS.with(|bindings| {
        *bindings.borrow_mut().get_mut(kind) =
            Some(Binding { vk: hotkey.vk(), modifiers: hotkey.required_modifiers(), down: false });
    });
    Ok(())
}

/// Stops watching for `kind`, removing the hook once nothing is left to watch
/// for.
///
/// A no-op for a kind that was never bound, which is what lets `window.rs`
/// call it unconditionally before every registration without first asking
/// which mode the previous one used.
pub(crate) fn unbind(kind: HotkeyKind) {
    let empty = BINDINGS.with(|bindings| {
        let mut bindings = bindings.borrow_mut();
        *bindings.get_mut(kind) = None;
        bindings.is_empty()
    });
    if empty {
        uninstall();
    }
}

/// Removes the hook and forgets every binding.
///
/// Called on the pump's way out. Windows would tear a hook down with the
/// thread that owns it, but leaving that to chance would mean the shutdown
/// path no longer says what it does — the same reasoning as the
/// `UnregisterHotKey` calls beside it.
pub(crate) fn uninstall() {
    HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
            tracing::info!("low-level keyboard hook removed");
        }
    });
    BINDINGS.with(|bindings| *bindings.borrow_mut() = Bindings::NONE);
    TARGET.with(|target| target.set(0));
}

/// Installs the hook, or does nothing if it is already installed.
fn install() -> Result<()> {
    HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        if hook.is_some() {
            return Ok(());
        }
        // `NULL` is documented as acceptable for a low-level hook whose
        // procedure lives in the calling process, but every long-lived example
        // of this API passes the module handle, and a wrong answer here is a
        // hook that installs and never fires.
        let module = unsafe { GetModuleHandleW(None) }
            .context("GetModuleHandleW failed while installing the keyboard hook")?;
        let installed = unsafe {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module.into()), 0)
        }
        .context(
            "SetWindowsHookExW(WH_KEYBOARD_LL) failed — the hotkey stays on the \
                     system hotkey table; set hotkey_mode = \"standard\" to stop trying",
        )?;
        *hook = Some(installed);
        tracing::info!("low-level keyboard hook installed");
        Ok(())
    })
}

/// The hook callback. Runs on the pump thread for every keystroke on the
/// machine, so it stays as short as it looks.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && code as u32 == HC_ACTION {
        // SAFETY: for `HC_ACTION` Windows guarantees `lparam` points at a
        // `KBDLLHOOKSTRUCT` that outlives this call. Only `vkCode` is read,
        // and it is copied out before anything else happens.
        let vk = unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode };
        match wparam.0 as u32 {
            // Sys- and non-sys are the same event as far as a combination is
            // concerned; which one arrives depends only on whether Alt is
            // held, and `alt+f10` would be invisible without both.
            WM_KEYDOWN | WM_SYSKEYDOWN => on_key_down(vk),
            WM_KEYUP | WM_SYSKEYUP => on_key_up(vk),
            _ => {}
        }
    }
    // Unconditional, including for the keys we just acted on: Trix listens, it
    // never intercepts. See this module's header for why that is the product
    // and not an omission.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn on_key_down(vk: u32) {
    let target = TARGET.with(Cell::get);
    if target == 0 {
        return;
    }
    // Asked once, not once per binding: it is the same keyboard either way,
    // and this runs on every keystroke on the machine.
    let live = live_modifiers();
    for kind in [HotkeyKind::Clip, HotkeyKind::Screenshot] {
        if !press(kind, vk, live) {
            continue;
        }
        // A post, not a call: the work a clip does is measured in seconds and
        // this callback's budget is 300 ms, after which Windows removes the
        // hook without telling anyone. `wnd_proc` picks it up from the same
        // queue the system hotkey table would have posted `WM_HOTKEY` to, and
        // runs the identical arm.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(target as *mut core::ffi::c_void)),
                WM_TRIX_HOOK_HOTKEY,
                WPARAM(kind.id() as usize),
                LPARAM(0),
            );
        }
    }
}

/// Marks `kind` held and reports whether this event is the press that did it —
/// false for a key that is not this combination, and for the auto-repeat of
/// one already held.
fn press(kind: HotkeyKind, vk: u32, live: Modifiers) -> bool {
    BINDINGS.with(|bindings| {
        // `try_borrow_mut` rather than `borrow_mut`: this callback is reentrant
        // in principle — it is called by the OS during message retrieval — and
        // a panicking borrow inside a hook procedure, in a workspace that
        // aborts on panic, would take the daemon down from a keystroke. Losing
        // one press is the better failure.
        let Ok(mut bindings) = bindings.try_borrow_mut() else {
            return false;
        };
        let Some(binding) = bindings.get_mut(kind).as_mut() else {
            return false;
        };
        if !matches(binding.vk, binding.modifiers, vk, live) || binding.down {
            return false;
        }
        binding.down = true;
        true
    })
}

/// Releases whichever combinations end in `vk`.
///
/// Keyed on the key itself and not on the whole combination: a user who lets
/// go of Alt before F10 has still finished pressing Alt+F10, and a release
/// that insisted on a full match would leave `down` stuck true and the hotkey
/// dead until the next reboot.
fn on_key_up(vk: u32) {
    BINDINGS.with(|bindings| {
        let Ok(mut bindings) = bindings.try_borrow_mut() else {
            return;
        };
        for kind in [HotkeyKind::Clip, HotkeyKind::Screenshot] {
            if let Some(binding) = bindings.get_mut(kind).as_mut()
                && binding.vk == vk
            {
                binding.down = false;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALT: Modifiers = Modifiers { ctrl: false, alt: true, shift: false, win: false };
    const NONE: Modifiers = Modifiers { ctrl: false, alt: false, shift: false, win: false };
    const VK_F10: u32 = 0x79;

    #[test]
    fn the_combination_fires_when_it_is_pressed() {
        assert!(matches(VK_F10, ALT, VK_F10, ALT));
    }

    #[test]
    fn another_key_with_the_same_modifiers_does_not_fire_it() {
        assert!(!matches(VK_F10, ALT, 0x78, ALT), "F9 is not F10");
    }

    #[test]
    fn the_key_alone_does_not_fire_a_combination_that_wants_a_modifier() {
        assert!(!matches(VK_F10, ALT, VK_F10, NONE));
    }

    /// The half of the rule a "does every required modifier hold?" check would
    /// get wrong. `RegisterHotKey` ignores Ctrl+Alt+F10 when Alt+F10 is what
    /// was registered, so low-level mode must ignore it too — otherwise
    /// switching modes silently widens what the user's hotkey answers to.
    #[test]
    fn an_extra_modifier_does_not_fire_it() {
        let ctrl_alt = Modifiers { ctrl: true, alt: true, shift: false, win: false };
        assert!(!matches(VK_F10, ALT, VK_F10, ctrl_alt));
    }

    #[test]
    fn a_bare_function_key_wants_no_modifiers_held() {
        assert!(matches(VK_F10, NONE, VK_F10, NONE));
        assert!(!matches(VK_F10, NONE, VK_F10, ALT), "Alt+F10 is not F10");
    }

    /// The repeat guard, which is what `MOD_NOREPEAT` buys the other mode.
    /// Held keys arrive as a stream of ordinary key-downs, and one clip per
    /// press is the contract.
    #[test]
    fn holding_the_key_down_fires_once() {
        BINDINGS.with(|bindings| {
            *bindings.borrow_mut().get_mut(HotkeyKind::Clip) =
                Some(Binding { vk: VK_F10, modifiers: ALT, down: false });
        });
        assert!(press(HotkeyKind::Clip, VK_F10, ALT), "the first press fires");
        assert!(!press(HotkeyKind::Clip, VK_F10, ALT), "auto-repeat does not");
        on_key_up(VK_F10);
        assert!(press(HotkeyKind::Clip, VK_F10, ALT), "releasing it re-arms");
        unbind(HotkeyKind::Clip);
    }

    /// Two rows, two bindings: a screenshot press must not be able to satisfy
    /// the clip binding's repeat guard, or holding one key would deafen the
    /// other.
    #[test]
    fn the_two_hotkeys_are_tracked_apart() {
        BINDINGS.with(|bindings| {
            let mut bindings = bindings.borrow_mut();
            *bindings.get_mut(HotkeyKind::Clip) =
                Some(Binding { vk: VK_F10, modifiers: ALT, down: false });
            *bindings.get_mut(HotkeyKind::Screenshot) =
                Some(Binding { vk: 0x77, modifiers: ALT, down: false });
        });
        assert!(press(HotkeyKind::Clip, VK_F10, ALT));
        assert!(press(HotkeyKind::Screenshot, 0x77, ALT), "still armed");
        on_key_up(VK_F10);
        assert!(!press(HotkeyKind::Screenshot, 0x77, ALT), "F10's release is not F8's");
        unbind(HotkeyKind::Clip);
        unbind(HotkeyKind::Screenshot);
    }

    /// An unbound kind never fires, which is what makes standard mode's
    /// unconditional `hook::unbind` before every registration safe.
    #[test]
    fn nothing_fires_once_it_is_unbound() {
        BINDINGS.with(|bindings| {
            *bindings.borrow_mut().get_mut(HotkeyKind::Clip) =
                Some(Binding { vk: VK_F10, modifiers: ALT, down: false });
        });
        unbind(HotkeyKind::Clip);
        assert!(!press(HotkeyKind::Clip, VK_F10, ALT));
    }
}
