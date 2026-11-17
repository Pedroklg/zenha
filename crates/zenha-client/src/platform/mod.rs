//! Integração com o sistema: bandeja, instância única, atalhos globais, área de transferência, credenciais e configurações.

pub mod clipboard_image;
pub mod credentials;
#[cfg(windows)]
pub mod global_keys;
pub mod hotkeys;
pub mod settings;
#[cfg(windows)]
pub mod single_instance;
pub mod tray;
