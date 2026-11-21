//! Safe Rust wrappers over Windows platform APIs: Credential Manager, system tray and global hotkeys.

pub mod credentials;
pub mod hotkeys;
pub mod tray;

pub(crate) fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
