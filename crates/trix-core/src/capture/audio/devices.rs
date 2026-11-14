//! Capture endpoint discovery. Listing devices never starts a capture stream.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use wasapi::{Device, DeviceEnumerator, Direction};

#[derive(Debug, Serialize)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn input_devices() -> Result<Vec<InputDevice>> {
    let _apartment = AudioApartment::enter()?;
    let enumerator = DeviceEnumerator::new().context("could not list microphones")?;
    // No default endpoint is normal when no microphones are connected.
    let default = enumerator.get_default_device(&Direction::Capture).and_then(|d| d.get_id()).ok();
    let collection = enumerator.get_device_collection(&Direction::Capture)?;
    let mut inputs = Vec::new();
    for index in 0..collection.get_nbr_devices()? {
        let device = collection.get_device_at_index(index)?;
        let id = device.get_id()?;
        inputs.push(InputDevice {
            is_default: default.as_deref() == Some(id.as_str()),
            id,
            name: device.get_friendlyname()?,
        });
    }
    Ok(inputs)
}

pub(super) fn capture_device_by_id(enumerator: &DeviceEnumerator, id: &str) -> Result<Device> {
    // wasapi 0.23's get_device drops its temporary HSTRING before GetDevice
    // uses the pointer. Enumerating also restricts selection to active inputs.
    let collection = enumerator.get_device_collection(&Direction::Capture)?;
    for index in 0..collection.get_nbr_devices()? {
        let device = collection.get_device_at_index(index)?;
        if device.get_id()? == id {
            return Ok(device);
        }
    }
    bail!("selected microphone is unavailable");
}

/// Balance COM initialization on repeated enumeration and capture failures.
pub(super) struct AudioApartment(bool);

impl AudioApartment {
    pub fn enter() -> Result<Self> {
        let hr = wasapi::initialize_mta();
        if hr == windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Ok(Self(false));
        }
        hr.ok().context("audio COM initialization failed")?;
        Ok(Self(true))
    }
}

impl Drop for AudioApartment {
    fn drop(&mut self) {
        if self.0 {
            wasapi::deinitialize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_selection_uses_exact_id_and_never_falls_back() {
        let _apartment = AudioApartment::enter().unwrap();
        let enumerator = DeviceEnumerator::new().unwrap();
        let default_before = enumerator
            .get_default_device(&Direction::Capture)
            .and_then(|device| device.get_id())
            .ok();
        for input in input_devices().unwrap() {
            let selected = capture_device_by_id(&enumerator, &input.id).unwrap();
            assert_eq!(selected.get_id().unwrap(), input.id);
            assert!(matches!(selected.get_direction(), Direction::Capture));
        }
        assert!(capture_device_by_id(&enumerator, "{not-a-real-endpoint}").is_err());
        assert_eq!(
            enumerator
                .get_default_device(&Direction::Capture)
                .and_then(|device| device.get_id())
                .ok(),
            default_before
        );
    }
}
