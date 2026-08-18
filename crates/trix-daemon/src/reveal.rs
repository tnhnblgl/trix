//! "Show in Explorer" — one window on the clip folder, not one per click.
//!
//! Its own module rather than more of [`crate::state`] because it is Win32 and
//! COM rather than daemon state, and because the thing it has to get right is a
//! shell contract, not a Trix rule.

use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};
use windows::core::PCWSTR;

use crate::com::Apartment;

/// Puts `file` in front of the user, selected.
///
/// If an Explorer window is already showing the folder, that window is the one
/// that comes forward — restored if it was minimised — with the selection moved
/// to this clip. Otherwise a window opens. What it does *not* do is take over a
/// window showing some other folder, and it does not stack up a second window
/// on the clip folder every time somebody presses the button. That last part is
/// why this module exists: `explorer.exe /select,` has no reuse mode at all, so
/// five reveals left five identical windows behind.
///
/// A failure to reach the shell falls back to exactly that old command, so the
/// worst case is the behaviour Trix already shipped rather than a button that
/// does nothing. The fallback earns its keep on paths [`SHParseDisplayName`]
/// refuses — see [`wide`] — and on the shell answering `ERROR_FILE_NOT_FOUND`
/// while it is busy, which it will do even when the folder is perfectly fine.
pub fn in_explorer(file: &Path) -> Result<()> {
    match select(file) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::debug!("reveal: the shell refused ({e:#}); opening a new window instead");
            new_window(file)
        }
    }
}

/// [`select_here`] on a thread of its own, and that is not incidental — it is
/// the rule [`crate::folder::pick`] documents at length. The shell is
/// apartment-threaded; the caller is a client thread that has been through the
/// dispatcher and may already be in an MTA, where
/// `CoInitializeEx(COINIT_APARTMENTTHREADED)` answers `RPC_E_CHANGED_MODE` and
/// the work gets marshalled into a host apartment instead. A thread that has
/// never touched COM cannot be in that state.
///
/// Joined rather than left to run, because the caller has an acknowledgement to
/// send and the fallback above has to know whether this worked. The wait is
/// affordable: measured on this machine, 230 ms with no Explorer window open at
/// all, and 22 ms when one is already up.
fn select(file: &Path) -> Result<()> {
    let file = file.to_path_buf();
    std::thread::Builder::new()
        .name("trix-reveal".into())
        .spawn(move || select_here(&file))
        .context("could not start the reveal thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("the reveal thread panicked"))?
}

/// The shell call itself, on a thread that owns its apartment.
///
/// Declaration order is load-bearing: locals drop in reverse, so the apartment
/// declared first is released last, after both [`Pidl`]s have gone. Freeing
/// shell memory after `CoUninitialize` would be handing it back to an allocator
/// that is no longer there.
fn select_here(file: &Path) -> Result<()> {
    let folder = file.parent().context("a clip path with no folder above it")?;
    let _apartment = Apartment::enter()?;

    let folder = Pidl::parse(folder)?;
    let item = Pidl::parse(file)?;
    unsafe { SHOpenFolderAndSelectItems(folder.0, Some(&[item.0.cast_const()]), 0) }
        .context("SHOpenFolderAndSelectItems")
}

/// The old behaviour, kept as the floor under [`in_explorer`].
///
/// Built argument by argument, never as a formatted command line: the id is
/// whitelisted upstream but the clip *directory* comes from user config and can
/// hold spaces, quotes, or an `&`, and handing that to a shell would be an
/// injection with the user's own token. `std::process::Command` passes the path
/// as one argument.
///
/// Explorer's exit code is not checked and the child is not waited on:
/// `explorer.exe /select,` routinely returns non-zero after opening the window
/// correctly, because it hands the request to the already-running shell process
/// and exits. Whether it *spawned* is the only thing that distinguishes "the
/// user is looking at their clip" from "nothing happened", so that is what is
/// reported.
fn new_window(file: &Path) -> Result<()> {
    std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(file)
        .spawn()
        .with_context(|| format!("could not open Explorer for {}", file.display()))?;
    Ok(())
}

/// An absolute shell id list, freed on drop.
///
/// `SHParseDisplayName` allocates with the task allocator and hands ownership
/// over, so the free is ours to do however the caller leaves — including the
/// early return where the *second* parse fails and the first has already
/// succeeded.
struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    fn parse(path: &Path) -> Result<Self> {
        let wide = wide(path);
        let mut pidl = std::ptr::null_mut();
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None) }
            .with_context(|| format!("the shell could not resolve {}", path.display()))?;
        Ok(Self(pidl))
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}

/// The path as a null-terminated UTF-16 string, with separators normalised.
///
/// `SHParseDisplayName` is stricter than `PathBuf`, and every form it rejects
/// costs the window reuse this module exists for. Measured against the real
/// shell: `C:\dir\clip.mp4` resolves; `C:/dir/clip.mp4` answers `E_INVALIDARG`;
/// and so does the extended form `std::fs::canonicalize` returns, which is why
/// nothing here canonicalises. Rust treats `/` as a separator on Windows, so a
/// hand-edited `clip_dir` in `config.toml` can hold one and every other part of
/// Trix works fine — only the shell objects. `/` cannot occur in a Windows file
/// name, so rewriting it is a normalisation and not a guess.
fn wide(path: &Path) -> Vec<u16> {
    const SLASH: u16 = b'/' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    path.as_os_str()
        .encode_wide()
        .map(|unit| if unit == SLASH { BACKSLASH } else { unit })
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_are_normalised_and_the_string_is_terminated() {
        let encoded = wide(Path::new("C:/Users/me/Videos/Trix/20260818_110251.mp4"));
        assert_eq!(encoded.last(), Some(&0), "the shell reads until a null");
        let text = String::from_utf16(&encoded[..encoded.len() - 1]).unwrap();
        assert_eq!(text, r"C:\Users\me\Videos\Trix\20260818_110251.mp4");
    }

    /// The load-bearing claim, checked against the real shell rather than
    /// asserted: what [`wide`] produces is a form `SHParseDisplayName` accepts.
    /// If it stops being one, every reveal quietly falls back to opening a new
    /// window and nothing else in the suite would notice.
    ///
    /// Parsing only — no window opens, because nothing here reaches
    /// `SHOpenFolderAndSelectItems`.
    #[test]
    fn the_shell_resolves_what_wide_produces() {
        let dir = std::env::temp_dir().join("trix-reveal-parse");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("20260818_110251.mp4");
        std::fs::write(&file, b"not really a clip").unwrap();

        let _apartment = Apartment::enter().unwrap();
        Pidl::parse(&file).expect("a backslash path should resolve");
        Pidl::parse(Path::new(&file.to_str().unwrap().replace('\\', "/")))
            .expect("a forward-slash path should resolve, because `wide` rewrites it");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Proves the fallback in [`in_explorer`] is reachable: a clip that is not
    /// there is an `Err` out of the shell, not a silent success.
    #[test]
    fn a_file_that_is_not_there_does_not_resolve() {
        let _apartment = Apartment::enter().unwrap();
        let missing = std::env::temp_dir().join("trix-reveal-no-such-clip").join("00000000.mp4");
        assert!(Pidl::parse(&missing).is_err(), "the shell resolved a path that is not there");
    }
}
