//! Global hotkey listener (Phase 5b, configurable since Phase 6a).
//!
//! `RegisterHotKey` binds to the registering thread's message queue, so a
//! dedicated thread owns the registration and pumps messages; each hotkey
//! press becomes one `()` on the returned channel.
//!
//! The combination is user-configurable (`clip_hotkey` in config.toml)
//! because no fixed key is safe from every overlay: NVIDIA's overlay consumes
//! its own Alt+F10 in games it has hooked before hotkey matching ever runs.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use anyhow::{Context as _, Result, anyhow, bail};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};
use windows::core::BOOL;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static FINALIZED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_console_ctrl(ctrl_type: u32) -> BOOL {
    SHUTDOWN.store(true, Ordering::Release);
    if ctrl_type != CTRL_C_EVENT && ctrl_type != CTRL_BREAK_EVENT {
        // Console close / logoff / OS shutdown kill the process the moment
        // this handler returns, so hold its thread while the session flushes
        // the MP4 (Windows force-kills after ~5 s regardless).
        for _ in 0..80 {
            if FINALIZED.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    BOOL::from(true)
}

/// Routes Ctrl+C / Ctrl+Break / console-close into a polled flag instead of
/// process death, so sessions can finalize their MP4 before exiting.
pub fn install_shutdown_handler() -> Result<()> {
    unsafe { SetConsoleCtrlHandler(Some(on_console_ctrl), true) }
        .context("SetConsoleCtrlHandler")
}

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::Acquire)
}

/// Signals the ctrl handler that on-disk state is consistent; a blocked
/// console-close handler returns (and lets Windows kill us) once this is set.
pub fn mark_finalized() {
    FINALIZED.store(true, Ordering::Release);
}

/// Refuses a second concurrent capture session: two would fight over the
/// hardware encoder and the clip hotkey. Session-local (`Local\`) so each
/// logged-in user gets their own slot. The mutex handle is deliberately not
/// closed — it must live for the whole process, and the OS reclaims it at
/// exit.
pub fn acquire_single_instance() -> Result<()> {
    let name = windows::core::HSTRING::from("Local\\trix-capture-single-instance");
    unsafe {
        let _handle = CreateMutexW(None, false, &name).context("CreateMutexW")?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            bail!(
                "another trix capture session (record or replay) is already \
                 running — stop it before starting a new one"
            );
        }
    }
    Ok(())
}

/// A parsed `mods+key` combination such as `alt+f10` or `ctrl+shift+c`.
#[derive(Clone, Debug)]
pub struct Hotkey {
    modifiers: HOT_KEY_MODIFIERS,
    vk: u32,
    pretty: String,
}

impl Hotkey {
    /// Accepts `[ctrl+][alt+][shift+][win+]key` where key is `a`–`z`,
    /// `0`–`9`, or `f1`–`f24`. Letters and digits require at least one
    /// modifier — a bare one would be swallowed system-wide while typing.
    pub fn parse(spec: &str) -> Result<Self> {
        let mut tokens: Vec<&str> = spec.split('+').map(str::trim).collect();
        let key_token = tokens.pop().unwrap_or("");
        let key = key_token.to_ascii_lowercase();

        let mut modifiers = HOT_KEY_MODIFIERS(0);
        let mut pretty = Vec::with_capacity(tokens.len() + 1);
        for token in tokens {
            let (flag, name) = match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => (MOD_CONTROL, "Ctrl"),
                "alt" => (MOD_ALT, "Alt"),
                "shift" => (MOD_SHIFT, "Shift"),
                "win" | "super" => (MOD_WIN, "Win"),
                other => {
                    bail!("unknown modifier {other:?} in hotkey {spec:?} (ctrl, alt, shift, win)")
                }
            };
            if modifiers.0 & flag.0 != 0 {
                bail!("modifier {name} appears twice in hotkey {spec:?}");
            }
            modifiers |= flag;
            pretty.push(name.to_string());
        }

        let fkey = key
            .strip_prefix('f')
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| (1..=24).contains(n));
        let vk = if let Some(n) = fkey {
            0x6F + n // VK_F1 == 0x70
        } else if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() {
            if modifiers.0 == 0 {
                bail!(
                    "hotkey {spec:?} needs a modifier — a bare letter or digit would be \
                     stolen from every application (try e.g. ctrl+alt+{key})"
                );
            }
            u32::from(key.as_bytes()[0].to_ascii_uppercase())
        } else {
            bail!(
                "cannot parse hotkey {spec:?}: expected mods+key like \"alt+f10\" or \
                 \"ctrl+shift+c\" (keys: a-z, 0-9, f1-f24)"
            );
        };
        pretty.push(key.to_ascii_uppercase());

        Ok(Self { modifiers, vk, pretty: pretty.join("+") })
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.pretty)
    }
}

pub fn start_hotkey(hotkey: &Hotkey) -> Result<Receiver<()>> {
    let (tx, rx) = channel();
    let (ready_tx, ready_rx) = channel::<Result<()>>();
    let modifiers = hotkey.modifiers | MOD_NOREPEAT;
    let vk = hotkey.vk;
    let pretty = hotkey.to_string();
    std::thread::Builder::new()
        .name("trix-hotkey".into())
        .spawn(move || unsafe {
            let registered = RegisterHotKey(None, 1, modifiers, vk);
            let ok = registered.is_ok();
            let _ = ready_tx.send(registered.map_err(|e| {
                anyhow!(
                    "RegisterHotKey {pretty} failed: {e} — another program already owns \
                     this combination; pick a different clip_hotkey in config.toml"
                )
            }));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default() {
        let hk = Hotkey::parse("alt+f10").unwrap();
        assert_eq!(hk.modifiers, MOD_ALT);
        assert_eq!(hk.vk, 0x79); // VK_F10
        assert_eq!(hk.to_string(), "Alt+F10");
    }

    #[test]
    fn parses_multi_modifier_letter() {
        let hk = Hotkey::parse("Ctrl + Shift + c").unwrap();
        assert_eq!(hk.modifiers, MOD_CONTROL | MOD_SHIFT);
        assert_eq!(hk.vk, u32::from(b'C'));
        assert_eq!(hk.to_string(), "Ctrl+Shift+C");
    }

    #[test]
    fn bare_function_key_allowed() {
        let hk = Hotkey::parse("f9").unwrap();
        assert_eq!(hk.modifiers.0, 0);
        assert_eq!(hk.vk, 0x78);
    }

    #[test]
    fn bare_letter_rejected() {
        assert!(Hotkey::parse("c").is_err());
    }

    #[test]
    fn nonsense_rejected() {
        assert!(Hotkey::parse("alt+f25").is_err());
        assert!(Hotkey::parse("meta+f1").is_err());
        assert!(Hotkey::parse("alt+alt+f1").is_err());
        assert!(Hotkey::parse("").is_err());
    }
}
