//! Ids dos dispositivos padrão do Windows (papel "console"), no formato que o ADM do WebRTC lista.
//!
//! O WebRTC, sem escolha explícita, usa o padrão de *comunicações*, que pode ser outro aparelho.

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
