//! Atualização automática via GitHub Releases: checagem de versão, download verificado e execução do instalador.

mod download;
mod install;
mod release;
#[cfg(test)]
mod tests;
mod version;

pub use download::{DownloadHandle, UpdateDownloadEvent, spawn_download};
pub use install::{
    cleanup_update_dir, installer_paths, is_installed_exe, launch_installer, update_temp_dir,
};
pub use release::{demo_available_update, spawn_check};
pub use version::should_show_update;

pub const DEFAULT_RELEASES_REPO: &str = "Pedroklg/zenha";
pub const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;
pub const INSTALLER_ARGS: &[&str] = &[
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/CLOSEAPPLICATIONS",
];

const USER_AGENT: &str = "Zenha";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallerAsset {
    pub url: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub url: String,
    pub installer: Option<InstallerAsset>,
}
