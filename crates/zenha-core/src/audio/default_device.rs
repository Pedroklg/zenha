//! Windows default device ids ("console" role), in the format the WebRTC ADM lists them.
//!
//! Without an explicit choice, WebRTC uses the *communications* default, which may be a different device.

use std::thread;

use wasapi::{Device, DeviceEnumerator, Direction, WasapiError, deinitialize, initialize_mta};

/// Opens the chosen device, or the Windows default when it is `None` or no longer exists
/// (a saved device that was removed would otherwise fail on every retry).
pub(crate) fn device_or_default(
    enumerator: &DeviceEnumerator,
    device_id: Option<&str>,
    direction: Direction,
) -> Result<Device, WasapiError> {
    if let Some(id) = device_id {
        match enumerator.get_device(id) {
            Ok(device) => return Ok(device),
            Err(err) => {
                tracing::warn!(%err, device_id = id, "saved audio device not found, using the windows default");
            }
        }
    }
    enumerator.get_default_device(direction)
}

pub(crate) fn default_input_id() -> Option<String> {
    default_id(false)
}

pub(crate) fn default_output_id() -> Option<String> {
    default_id(true)
}

/// COM is initialised on a fresh thread so no apartment state leaks into the caller.
fn default_id(render: bool) -> Option<String> {
    thread::spawn(move || {
        if initialize_mta().is_err() {
            return None;
        }
        let direction = if render {
            Direction::Render
        } else {
            Direction::Capture
        };
        let id = DeviceEnumerator::new()
            .and_then(|enumerator| enumerator.get_default_device(direction))
            .and_then(|device| device.get_id())
            .ok();
        deinitialize();
        id
    })
    .join()
    .ok()
    .flatten()
}
