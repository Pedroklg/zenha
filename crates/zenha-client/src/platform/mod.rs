//! System integration: tray, single instance, global hotkeys, clipboard, credentials and settings.

pub mod clipboard_image;
pub mod credentials;
pub mod diagnostics;
pub mod error_report;
#[cfg(windows)]
pub mod global_keys;
pub mod hotkeys;
pub mod logging;
pub mod settings;
#[cfg(windows)]
pub mod single_instance;
pub mod tray;
