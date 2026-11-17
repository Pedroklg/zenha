//! Windows default device ids ("console" role), in the format the WebRTC ADM lists them.
//!
//! Without an explicit choice, WebRTC uses the *communications* default, which may be a different device.

use std::thread;

use wasapi::{DeviceEnumerator, Direction, deinitialize, initialize_mta};

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
