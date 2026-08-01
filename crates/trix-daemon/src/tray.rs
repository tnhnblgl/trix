//! The tray icon, its menu, and the armed/idle artwork (spec §7.2).
//!
//! Everything here runs on the window's pump thread. Tray icons belong to the
//! thread that owns the window they post to, and `TrackPopupMenu` runs its own
//! modal loop that only works on a thread with a message queue — so the `Tray`
//! is created, updated, and destroyed by `window.rs`'s pump and never touched
//! from the worker. The worker asks for a state change by posting a message.

use anyhow::{Context as _, Result, anyhow};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_MESSAGE,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, HICON,
    MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTALIGN,
    TrackPopupMenu, WM_APP, WM_NULL,
};
use windows::core::{HSTRING, PCWSTR};

/// The tray callback message. Anything from `WM_APP` up is ours.
pub const WM_TRIX_TRAY: u32 = WM_APP + 1;

/// Menu command ids, returned by `TrackPopupMenu`.
pub const ID_TOGGLE: usize = 1;
pub const ID_OPEN_UI: usize = 2;
pub const ID_OPEN_FOLDER: usize = 3;
pub const ID_QUIT: usize = 4;

/// Icon edge in pixels. 32 is the large-DPI tray size; Windows downscales to
/// 16 cleanly and asking for a 16 would look soft at 150% scaling.
pub(crate) const ICON_SIDE: usize = 32;

/// A ring (idle) or a filled disc (armed), as premultiplied BGRA.
///
/// Drawn rather than shipped as an `.ico`: two circles need no resource
/// compiler, no asset files, and no build script, and stay correct if the
/// binary is moved. The shape is deliberately the same silhouette in both
/// states so the icon reads as "Trix" either way — only the fill changes.
pub(crate) fn icon_pixels(armed: bool) -> Vec<u32> {
    let side = ICON_SIDE as f32;
    let center = (side - 1.0) / 2.0;
    let outer = side * 0.44;
    let inner = outer * 0.55;
    let mut pixels = vec![0u32; ICON_SIDE * ICON_SIDE];
    for y in 0..ICON_SIDE {
        for x in 0..ICON_SIDE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let d = (dx * dx + dy * dy).sqrt();
            // One-pixel smoothstep at each edge: an aliased circle at 16 px
            // looks like a defect.
            let edge = |r: f32| (1.0 - (d - r + 0.5)).clamp(0.0, 1.0);
            let coverage = if armed { edge(outer) } else { edge(outer) * (1.0 - edge(inner)) };
            let alpha = (coverage * 255.0) as u32;
            // Premultiplied white; the tray composites over unknown
            // backgrounds and a non-premultiplied icon fringes dark.
            pixels[y * ICON_SIDE + x] = (alpha << 24) | (alpha << 16) | (alpha << 8) | alpha;
        }
    }
    pixels
}

/// A live tray icon. Removing it is not optional: an icon whose process dies
/// without `NIM_DELETE` lingers in the tray until the user happens to hover
/// it, which looks exactly like a crashed app.
pub struct Tray {
    hwnd: HWND,
    icons: [HICON; 2],
    armed: bool,
}

impl Tray {
    /// # Safety
    /// Must be called on the thread that owns `hwnd`, and the `Tray` must be
    /// dropped on that same thread.
    pub unsafe fn add(hwnd: HWND) -> Result<Self> {
        let icons = [unsafe { make_icon(false) }?, unsafe { make_icon(true) }?];
        let tray = Self { hwnd, icons, armed: false };
        // A failed `NIM_ADD` is kept rather than propagated, because its most
        // likely cause is benign and self-correcting: the daemon autostarts at
        // login (§7.3) and can beat `explorer.exe` to the desktop, so there is
        // no taskbar to add an icon to yet. `readd` runs when the shell
        // announces itself. Only a failure to build the icons is fatal.
        if let Err(e) = unsafe { tray.notify(NIM_ADD) } {
            tracing::warn!(
                error = %format!("{e:#}"),
                "the tray icon did not take; retrying when the shell announces itself"
            );
        }
        Ok(tray)
    }

    /// Re-adds the icon after the shell restarts.
    ///
    /// Windows broadcasts `TaskbarCreated` when `explorer.exe` starts, and
    /// every tray icon that existed before is gone. Without this the daemon
    /// keeps running with no icon — invisible, and looking crashed. It covers
    /// two real cases: explorer restarting (common on the low-end machines
    /// this project targets) and the autostart race above.
    pub fn readd(&self) {
        if let Err(e) = unsafe { self.notify(NIM_ADD) } {
            tracing::warn!(error = %format!("{e:#}"), "could not re-add the tray icon");
        } else {
            tracing::debug!("re-added the tray icon after the shell restarted");
        }
    }

    /// Swaps the icon and the tooltip. Cheap enough to call on every state
    /// change; a `NIM_MODIFY` that changes nothing is skipped outright.
    pub fn set_armed(&mut self, armed: bool) {
        if self.armed == armed {
            return;
        }
        self.armed = armed;
        match unsafe { self.notify(NIM_MODIFY) } {
            // The tray icon is the daemon's only output on a machine with no
            // console, so its state changes are logged: "the icon is wrong" is
            // otherwise an unfalsifiable bug report.
            Ok(()) => tracing::debug!(armed, "tray icon updated"),
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "could not update the tray icon"),
        }
    }

    unsafe fn notify(&self, message: NOTIFY_ICON_MESSAGE) -> Result<()> {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRIX_TRAY,
            hIcon: self.icons[usize::from(self.armed)],
            ..Default::default()
        };
        // The tooltip is the only text the user gets for "is it recording?"
        // without opening the menu, so it names the state outright.
        let tip = if self.armed { "Trix — armed" } else { "Trix — idle" };
        for (i, c) in tip.encode_utf16().enumerate().take(data.szTip.len() - 1) {
            data.szTip[i] = c;
        }
        unsafe { Shell_NotifyIconW(message, &data) }
            .ok()
            .map_err(|e| anyhow!("Shell_NotifyIconW failed: {e}"))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = self.notify(NIM_DELETE);
            for icon in self.icons {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

unsafe fn make_icon(armed: bool) -> Result<HICON> {
    let pixels = icon_pixels(armed);
    // CreateIcon takes separate AND and XOR masks; with a full alpha channel
    // in the colour bits the AND mask is ignored, so it is all-zero.
    let and_mask = vec![0u8; ICON_SIDE * ICON_SIDE / 8];
    let xor: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
    unsafe {
        CreateIcon(None, ICON_SIDE as i32, ICON_SIDE as i32, 1, 32, and_mask.as_ptr(), xor.as_ptr())
    }
    .context("CreateIcon")
}

/// Shows the tray menu at the cursor and returns the chosen command id.
///
/// `SetForegroundWindow` before `TrackPopupMenu` is required, not decorative:
/// without it the menu does not dismiss when the user clicks elsewhere and
/// stays stuck on screen. The `WM_NULL` afterwards is the documented other
/// half of the same workaround.
///
/// # Safety
/// Must be called on the thread that owns `hwnd`.
pub unsafe fn show_menu(hwnd: HWND, armed: bool) -> Option<usize> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let toggle = if armed { "Disarm" } else { "Arm" };
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, &HSTRING::from(toggle));
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_UI, &HSTRING::from("Open Trix"));
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_FOLDER, &HSTRING::from("Open clips folder"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, &HSTRING::from("Quit"));

        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RIGHTALIGN | TPM_RETURNCMD,
            cursor.x,
            cursor.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, Default::default(), Default::default());
        let _ = DestroyMenu(menu);
        match chosen.0 {
            0 => None,
            id => Some(id as usize),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two states must be visually distinct — an armed daemon that looks
    /// identical to an idle one is the tray icon failing at its only job.
    ///
    /// Asserts the shape rather than a pixel count. The plan's original
    /// `armed_on > idle_on * 2` is arithmetically unreachable: with an inner
    /// radius of `0.55 × outer` the ring covers ~70% of the disc's area, not
    /// under half, so that test could only ever have failed.
    #[test]
    fn armed_and_idle_icons_differ_and_are_both_drawn() {
        let idle = icon_pixels(false);
        let armed = icon_pixels(true);
        assert_eq!(idle.len(), ICON_SIDE * ICON_SIDE, "one BGRA pixel per cell");
        assert_eq!(armed.len(), idle.len());

        let alpha = |px: &[u32], x: usize, y: usize| px[y * ICON_SIDE + x] >> 24;
        let opaque = |px: &[u32]| px.iter().filter(|p| (*p >> 24) > 0x40).count();
        let mid = ICON_SIDE / 2;

        // The entire difference between the two states, in two assertions:
        // the middle is empty when idle and filled when armed.
        assert!(alpha(&idle, mid, mid) < 0x20, "the idle icon must be hollow in the middle");
        assert!(alpha(&armed, mid, mid) > 0xE0, "the armed icon must be filled in the middle");

        // Same silhouette either way, so the icon still reads as Trix in both
        // states — only the fill changes.
        let rim = mid + (ICON_SIDE as f32 * 0.44) as usize - 1;
        assert!(alpha(&idle, rim, mid) > 0x40, "the idle icon must draw its rim");
        assert!(alpha(&armed, rim, mid) > 0x40, "the armed icon must draw its rim");
        assert!(opaque(&armed) > opaque(&idle), "filled must cover more than hollow");

        // A fully opaque corner means the alpha channel was lost somewhere,
        // which shows up in the tray as a white square.
        assert_eq!(alpha(&idle, 0, 0), 0, "corners must be transparent");
        assert_eq!(alpha(&armed, 0, 0), 0, "corners must be transparent");
    }
}
