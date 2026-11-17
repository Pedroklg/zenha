//! Atualização automática via GitHub Releases: checagem de versão, download verificado e execução do instalador.

mod download;
mod install;
mod release;
#[cfg(test)]
mod tests;
mod version;

pub use download::{DownloadError, DownloadHandle, PartVerifier, UpdateDownloadEvent, spawn_download};
pub use install::{
    cleanup_update_dir, installer_paths, is_installed_exe, is_installed_exe_path, launch_installer,
    normalize_windows_path, update_temp_dir,
};
pub use release::{
    ReleaseAsset, demo_available_update, evaluate_release, installer_asset_name,
    is_safe_repo_download_url, latest_release_url, parse_sha256_digest, resolve_releases_repo,
    select_installer_asset, spawn_check,
};
pub use version::{SemVer, next_demo_version, parse_semver, should_show_update};

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
