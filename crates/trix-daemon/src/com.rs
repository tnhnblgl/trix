//! The COM apartment guard, shared by the two places in the daemon that talk
//! to the shell: [`crate::folder`]'s file dialogs and [`crate::reveal`]'s
//! "Show in Explorer".
//!
//! Its own module because neither of those is the natural owner of the
//! other's apartment, and because a guard whose whole job is to undo exactly
//! what it did is easiest to check when it is the only thing in the file.

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
/// Every caller gives itself a fresh thread, so only the first of those can
/// happen. The other two are handled anyway — the cost is four lines, and the
/// failures they prevent are a folder picker that never appears and an
/// apartment torn down under whoever else was using it.
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
