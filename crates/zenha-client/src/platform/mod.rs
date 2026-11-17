//! System integration: tray, single instance, global hotkeys, clipboard, credentials and settings.

pub mod clipboard_image;
pub mod credentials;
#[cfg(windows)]
pub mod global_keys;
pub mod hotkeys;
pub mod settings;
#[cfg(windows)]
pub mod single_instance;
pub mod tray;
