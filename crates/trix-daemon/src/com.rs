//! The COM apartment guard, shared by three callers in the daemon:
//! [`crate::folder`]'s file dialogs, [`crate::reveal`]'s "Show in Explorer",
//! and [`crate::state::Daemon::shots_copy`]'s WIC decode of a saved
//! screenshot. The third has nothing to do with the shell — it needs the same
//! "make sure this thread has an apartment" guarantee WIC requires as much as
//! `SHOpenFolderAndSelectItems` and the file-open dialog do.
//!
//! Its own module because no one of those three is the natural owner of
//! either of the others' apartments, and because a guard whose whole job is
//! to undo exactly what it did is easiest to check when it is the only thing
//! in the file.

use anyhow::Result;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

/// COM initialised for the duration of a call, and undone exactly when it was
/// this guard that did it.
///
/// `CoInitializeEx` has three outcomes needing three behaviours, which is why
/// this is a guard rather than a bare pair of calls: `S_OK` means we
/// initialised the apartment and owe a `CoUninitialize`; `S_FALSE` means it was
/// already initialised on this thread and we *still* owe one, because the
/// count is per call; and `RPC_E_CHANGED_MODE` means the thread is already an
/// MTA member, where uninitialising would tear down an apartment we do not own.
///
/// A fresh thread is the common case — every `folder.rs` dialog and
/// `reveal.rs`'s "Show in Explorer" each spawn their own before calling this,
/// so `S_OK` is all they ever see. `shots_copy` breaks that pattern: it runs
/// on a long-lived dispatcher client thread that can call in more than once,
/// which is exactly when `S_FALSE` (this thread already holds an apartment
/// from an earlier call) and `RPC_E_CHANGED_MODE` (something else on this
/// thread already put it in an MTA) stop being the two outcomes nothing ever
/// hits. They are handled all the same regardless of caller — the cost is
/// four lines, and the failures they prevent are a folder picker that never
/// appears and an apartment torn down under whoever else was using it.
pub struct Apartment {
    owned: bool,
}

impl Apartment {
    pub fn enter() -> Result<Self> {
        // Apartment-threaded, as the shell requires.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            return Ok(Self { owned: true });
        }
        if hr == windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Ok(Self { owned: false });
        }
        Err(anyhow::anyhow!("CoInitializeEx failed: {hr:?}"))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}
