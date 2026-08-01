//! Opt-in autostart through `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! The registry is the source of truth rather than `config.toml`: a user who
//! removes the Run entry with regedit or a startup manager has disabled
//! autostart, and a config file claiming otherwise would be lying to the
//! settings page. `config.get` therefore reports what this module reads, not
//! what the file holds (spec §7.3).
//!
//! `HKCU` needs no elevation, which is the whole reason autostart is per-user
//! and not a service.

use anyhow::{Context as _, Result};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::HSTRING;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The Run value name. Also what the user sees in Task Manager's Startup tab.
pub const VALUE_NAME: &str = "Trix";

/// True if the daemon is registered to start at login.
pub fn is_enabled() -> bool {
    is_enabled_at(RUN_KEY, VALUE_NAME)
}

/// Adds or removes the Run entry. Idempotent in both directions.
pub fn set_enabled(enabled: bool) -> Result<()> {
    set_enabled_at(RUN_KEY, VALUE_NAME, enabled)
}

/// An open handle that closes itself. `set_enabled_at` has several failure
/// paths after the key is open, and a `?` on any of them would otherwise leak
/// the handle — this daemon runs for a whole gaming session and the leak would
/// be per-toggle.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open_key(path: &str, access: REG_SAM_FLAGS) -> Result<Key> {
    let mut key = HKEY::default();
    let status =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, access, &mut key) };
    if status != ERROR_SUCCESS {
        return Err(win32(status)).with_context(|| format!("opening HKCU\\{path}"));
    }
    Ok(Key(key))
}

/// Opens `path`, creating it if it is not there. Only the write path needs
/// this: the real Run key always exists, but a test's scratch key does not,
/// and creating on demand is what lets the key path be a parameter at all.
fn create_key(path: &str) -> Result<Key> {
    let mut key = HKEY::default();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE | KEY_READ,
            None,
            &mut key,
            None,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32(status)).with_context(|| format!("creating HKCU\\{path}"));
    }
    Ok(Key(key))
}

fn win32(status: WIN32_ERROR) -> windows::core::Error {
    windows::core::Error::from(status.to_hresult())
}

/// Reads as off when the key or the value is missing, and when the read fails
/// outright. There is no third answer to give a settings page: a Run entry
/// that cannot be read is not going to start anything at login either.
fn is_enabled_at(key_path: &str, name: &str) -> bool {
    let Ok(key) = open_key(key_path, KEY_READ) else { return false };
    unsafe {
        RegQueryValueExW(key.0, &HSTRING::from(name), None, None, None, None) == ERROR_SUCCESS
    }
}

fn set_enabled_at(key_path: &str, name: &str, enabled: bool) -> Result<()> {
    if !enabled {
        // Nothing to remove from a key that is not there. Opening for write
        // would create nothing and fail, and a user who cleaned out the entry
        // by hand must still be able to toggle the setting off.
        let key = match open_key(key_path, KEY_WRITE) {
            Ok(key) => key,
            Err(_) if !is_enabled_at(key_path, name) => return Ok(()),
            Err(e) => return Err(e),
        };
        let status = unsafe { RegDeleteValueW(key.0, &HSTRING::from(name)) };
        return match status {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            status => Err(win32(status)).context("could not remove the Run value"),
        };
    }

    // Quoted: a path with spaces (`C:\Program Files\...`) in an unquoted Run
    // value is parsed as a command plus arguments.
    let exe = std::env::current_exe().context("resolving the daemon path")?;
    let command = format!("\"{}\"", exe.display());
    // REG_SZ is bytes on the wire, and the terminating NUL is part of the
    // value: without it, whatever read the value back would run off the end.
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };

    let key = create_key(key_path)?;
    let status = unsafe { RegSetValueExW(key.0, &HSTRING::from(name), None, REG_SZ, Some(bytes)) };
    match status {
        ERROR_SUCCESS => Ok(()),
        status => Err(win32(status)).context("could not write the Run value"),
    }
}

/// Test-only: removes a key outright. `RegDeleteKeyW` deletes only an empty
/// key, which is what makes it safe to point at the parent as well.
#[cfg(test)]
fn delete_key(path: &str) -> Result<()> {
    use windows::Win32::System::Registry::RegDeleteKeyW;

    let status = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(path)) };
    match status {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        status => Err(win32(status)).with_context(|| format!("deleting HKCU\\{path}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removes the scratch key on the way out **including when an assertion
    /// panics**. Without this, every failing run leaves an orphan key behind —
    /// which is exactly what happened while this test was being falsified.
    /// Tests unwind: `panic = "abort"` is set on the release profile only.
    struct Scratch(String);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = delete_key(&self.0);
            // Created as a side effect of the scratch key and not ours to
            // keep. `RegDeleteKeyW` only removes an empty key, so this is a
            // no-op the moment anything else lives under it.
            let _ = delete_key(r"Software\Trix");
        }
    }

    /// Round-trips through the real registry, but under a scratch key of this
    /// test's own — never `HKCU\...\Run`.
    ///
    /// Writing to the real Run key under a test-only *value* name would still
    /// put a startup entry pointing at the test binary into the developer's
    /// actual startup list, and a panic between the write and the cleanup
    /// would leave it there. That is the same class of defect commit 5b45e9a
    /// removed when it stopped ten unit tests running against real files. The
    /// key path is a parameter for exactly this reason; the production path
    /// differs only in which key it names, and
    /// [`the_key_the_daemon_writes_to_actually_exists`] covers that name.
    ///
    /// `HKCU` needs no elevation, so this runs anywhere.
    #[test]
    fn autostart_round_trips_and_removing_it_twice_is_not_an_error() {
        let key = format!(r"Software\Trix\autostart-test-{}", std::process::id());
        let scratch = Scratch(key.clone());
        let _ = delete_key(&key);

        assert!(!is_enabled_at(&key, VALUE_NAME), "a key that does not exist yet reads as off");

        set_enabled_at(&key, VALUE_NAME, true).expect("writing the Run value");
        assert!(is_enabled_at(&key, VALUE_NAME), "must read back as on");

        set_enabled_at(&key, VALUE_NAME, false).expect("removing the value");
        assert!(!is_enabled_at(&key, VALUE_NAME), "must read as off once removed");

        // A user who deleted the entry by hand with regedit must still be able
        // to toggle the setting off without being shown a failure.
        set_enabled_at(&key, VALUE_NAME, false).expect("removing twice is not an error");

        drop(scratch);
        assert!(!is_enabled_at(&key, VALUE_NAME), "the scratch key is gone");
    }

    /// The test above is safe *because* a missing key reads as off — which
    /// means a typo in [`RUN_KEY`] would make autostart silently report off
    /// forever and never fail a test. This is the check that catches that.
    #[test]
    fn the_key_the_daemon_writes_to_actually_exists() {
        open_key(RUN_KEY, KEY_READ).expect("HKCU Run did not open for reading");
    }
}
