//! The daemon's file dialogs — "Change clips folder…" and "choose a clip
//! sound" — and the one message box that reports them failing.
//!
//! Its own module rather than more of `window.rs` because it is the daemon's
//! only piece of real user interface: everything else there is a message pump,
//! a tray icon, and a channel. This is COM, and it blocks for as long as a
//! person takes to browse a filesystem.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog,
    IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MESSAGEBOX_STYLE, MessageBoxW};
use windows::core::{HSTRING, PCWSTR};

use crate::com::Apartment;

/// Shows the folder picker, starting at `current`.
///
/// `Ok(None)` is a cancel, which is a normal outcome and not an error — the
/// caller must not report it. `Err` is the dialog itself failing.
///
/// Blocks until the user is finished browsing, which may be a minute, so the
/// caller must be somewhere that can afford to wait — the worker thread, never
/// the message pump.
///
/// **Always on a thread of its own**, and that is not incidental. The shell's
/// dialogs are apartment-threaded, while the caller is the worker thread, which
/// has by then run arms and clips through Media Foundation. If any of that left
/// the thread in an MTA, `CoInitializeEx(COINIT_APARTMENTTHREADED)` answers
/// `RPC_E_CHANGED_MODE` and the dialog gets created in a host apartment and
/// marshalled — which is how folder pickers end up invisible, unresponsive, or
/// behind the game. A thread that has never touched COM cannot be in that
/// state, and one thread per menu click costs nothing at human scale.
pub fn pick(current: &Path) -> Result<Option<PathBuf>> {
    let current = current.to_path_buf();
    std::thread::Builder::new()
        .name("trix-folder-picker".into())
        .spawn(move || show(&current))
        .context("could not start the folder-picker thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("the folder-picker thread panicked"))?
}

/// The dialog itself, on a thread that owns its apartment. See [`pick`].
///
/// Deliberately **unowned**. The obvious owner would be the daemon's window,
/// and it is the wrong one: that window is `HWND_MESSAGE`, which by definition
/// has no z-order and never becomes visible, so a dialog owned by it inherits a
/// position in no z-order at all. An unowned dialog is a normal top-level
/// window that activates normally — and the click that opened the tray menu has
/// already given this process the right to come to the foreground.
fn show(current: &Path) -> Result<Option<PathBuf>> {
    let _apartment = Apartment::enter()?;

    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
            .context("could not create the folder picker")?;

    // FORCEFILESYSTEM keeps the result to somewhere clips can actually be
    // written: without it the picker will happily return a virtual shell
    // location — "This PC", a library, a phone plugged in over MTP — that has
    // no filesystem path at all.
    let options = unsafe { dialog.GetOptions() }.context("IFileDialog::GetOptions")?;
    unsafe {
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)
    }
    .context("IFileDialog::SetOptions")?;
    let _ = unsafe { dialog.SetTitle(&HSTRING::from("Choose where Trix saves clips")) };

    // Open where the clips go today, so "change" starts from the current
    // answer rather than from wherever the shell last left off. Best-effort:
    // a folder that no longer exists is exactly when the user needs this
    // dialog most, and must not be the reason it refuses to open.
    if let Ok(item) =
        unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(current), None) }
    {
        let _ = unsafe { dialog.SetFolder(&item) };
    }

    if let Err(e) = unsafe { dialog.Show(None) } {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            return Ok(None);
        }
        return Err(anyhow::Error::from(e).context("the folder picker failed"));
    }

    chosen_path(&dialog)
}

/// Pulls the chosen filesystem path out of a dialog the user accepted.
///
/// Shared by both dialogs because the shell's ownership rule is the part worth
/// writing once: `GetDisplayName` hands back memory the shell allocated, and it
/// is ours to free whatever else happens — so the string is copied out before
/// anything can return early.
fn chosen_path(dialog: &IFileOpenDialog) -> Result<Option<PathBuf>> {
    let item = unsafe { dialog.GetResult() }.context("IFileOpenDialog::GetResult")?;
    let wide = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
        .context("the chosen item has no filesystem path")?;
    let chosen = unsafe { wide.to_string() };
    unsafe { CoTaskMemFree(Some(wide.0 as *const _)) };
    Ok(Some(PathBuf::from(chosen.context("the chosen path is not valid UTF-16")?)))
}

/// Shows the "choose a clip sound" dialog.
///
/// `Ok(None)` is a cancel — a normal outcome the caller must not report.
///
/// On its own thread with its own apartment for exactly the reasons [`pick`]
/// documents at length: the shell's dialogs are apartment-threaded, and a
/// caller that has already run Media Foundation may be in an MTA, which is how
/// pickers end up invisible or behind the game.
pub fn pick_sound() -> Result<Option<PathBuf>> {
    std::thread::Builder::new()
        .name("trix-sound-picker".into())
        .spawn(show_sound)
        .context("could not start the sound-picker thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("the sound-picker thread panicked"))?
}

fn show_sound() -> Result<Option<PathBuf>> {
    let _apartment = Apartment::enter()?;

    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
            .context("could not create the sound picker")?;

    let options = unsafe { dialog.GetOptions() }.context("IFileDialog::GetOptions")?;
    unsafe { dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST) }
        .context("IFileDialog::SetOptions")?;
    let _ = unsafe { dialog.SetTitle(&HSTRING::from("Choose the sound Trix plays for a clip")) };

    // The filter is a convenience, not the rule -- the decoder is what actually
    // decides. "All files" is second so somebody with an .opus or an .aiff can
    // still try it, and find out from the error rather than from a dialog that
    // refuses to show them their own file.
    let audio = HSTRING::from("Audio files");
    let audio_spec = HSTRING::from("*.mp3;*.wav;*.m4a;*.wma;*.flac");
    let all = HSTRING::from("All files");
    let all_spec = HSTRING::from("*.*");
    let filters = [
        COMDLG_FILTERSPEC { pszName: PCWSTR(audio.as_ptr()), pszSpec: PCWSTR(audio_spec.as_ptr()) },
        COMDLG_FILTERSPEC { pszName: PCWSTR(all.as_ptr()), pszSpec: PCWSTR(all_spec.as_ptr()) },
    ];
    let _ = unsafe { dialog.SetFileTypes(&filters) };

    if let Err(e) = unsafe { dialog.Show(None) } {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            return Ok(None);
        }
        return Err(anyhow::Error::from(e).context("the sound picker failed"));
    }

    chosen_path(&dialog)
}

/// Tells the user why the folder they picked was refused.
///
/// A message box rather than a log line or a tray balloon: they are standing at
/// a dialog they just dismissed, waiting to find out whether it worked. A
/// balloon can be suppressed by Focus Assist — which is on during exactly the
/// full-screen games this daemon exists for — and a log line in `%APPDATA%` is
/// not an answer to a question someone asked ten seconds ago.
///
/// Unowned, for the same reason [`show`] is.
pub fn report_error(message: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            &HSTRING::from("Trix"),
            MESSAGEBOX_STYLE(MB_OK.0 | MB_ICONERROR.0),
        )
    };
}
