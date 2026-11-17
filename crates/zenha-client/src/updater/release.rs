//! Consulta à última release no GitHub e escolha do instalador.

use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use serde::Deserialize;

use super::version::{next_demo_version, parse_semver};
use super::{
    AvailableUpdate, DEFAULT_RELEASES_REPO, InstallerAsset, MAX_INSTALLER_BYTES, USER_AGENT,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
    pub browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

pub fn resolve_releases_repo(env_value: Option<&str>) -> &str {
    env_value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_RELEASES_REPO)
}

pub fn latest_release_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

pub fn installer_asset_name(version: &str) -> String {
    format!("Zenha-{version}-windows-x64-setup.exe")
}

pub fn parse_sha256_digest(raw: &str) -> Option<[u8; 32]> {
    let hex = raw.trim().strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_safe_http_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .is_some_and(|rest| !rest.is_empty())
}

fn parse_repo(repo: &str) -> Option<(&str, &str)> {
    let trimmed = repo.trim().trim_matches('/');
    let (owner, name) = trimmed.split_once('/')?;
    if owner.is_empty()
        || name.is_empty()
        || name.contains('/')
        || trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
        || owner == "."
        || owner == ".."
        || name == "."
        || name == ".."
    {
        return None;
    }
    Some((owner, name))
}

pub fn is_safe_repo_download_url(url: &str, repo: &str) -> bool {
    let trimmed = url.trim();
    if !is_safe_http_url(trimmed) {
        return false;
    }
    let Some(after_scheme) = trimmed.strip_prefix("https://") else {
        return false;
    };
    let Some((host, rest)) = after_scheme.split_once('/') else {
        return false;
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return false;
    }
    let Some((expected_owner, expected_name)) = parse_repo(repo) else {
        return false;
    };
    let Some((url_owner, after_owner)) = rest.split_once('/') else {
        return false;
    };
    let Some((url_name, tail)) = after_owner.split_once('/') else {
        return false;
    };
    if !url_owner.eq_ignore_ascii_case(expected_owner)
        || !url_name.eq_ignore_ascii_case(expected_name)
    {
        return false;
    }
    if tail.is_empty() || tail.contains(['\\', '?', '#']) {
        return false;
    }
    !tail
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

pub fn select_installer_asset(
    version: &str,
    repo: &str,
    assets: &[ReleaseAsset],
) -> Option<InstallerAsset> {
    let expected_name = installer_asset_name(version);
    let asset = assets.iter().find(|item| item.name == expected_name)?;
    if asset.size == 0 || asset.size > MAX_INSTALLER_BYTES {
        return None;
    }
    let sha256 = parse_sha256_digest(asset.digest.as_deref()?)?;
    if !is_safe_repo_download_url(&asset.browser_download_url, repo) {
        return None;
    }
    Some(InstallerAsset {
        url: asset.browser_download_url.trim().to_owned(),
        size: asset.size,
        sha256,
    })
}

pub fn evaluate_release(
    current_version: &str,
    repo: &str,
    tag_name: &str,
    html_url: &str,
    prerelease: bool,
    draft: bool,
    assets: &[ReleaseAsset],
) -> Option<AvailableUpdate> {
    if prerelease || draft {
        return None;
    }
    let current = parse_semver(current_version)?;
    let latest = parse_semver(tag_name)?;
    if latest <= current {
        return None;
    }
    let url = html_url.trim();
    if !is_safe_http_url(url) {
        return None;
    }
    let version = latest.to_string();
    let installer = select_installer_asset(&version, repo, assets);
    Some(AvailableUpdate {
        version,
        url: url.to_owned(),
        installer,
    })
}

pub fn demo_available_update() -> AvailableUpdate {
    let version = next_demo_version(env!("CARGO_PKG_VERSION"));
    let asset_name = installer_asset_name(&version);
    AvailableUpdate {
        url: format!("https://github.com/{DEFAULT_RELEASES_REPO}/releases/tag/v{version}"),
        installer: Some(InstallerAsset {
            url: format!(
                "https://github.com/{DEFAULT_RELEASES_REPO}/releases/download/v{version}/{asset_name}"
            ),
            size: 24 * 1024 * 1024,
            sha256: [0u8; 32],
        }),
        version,
    }
}

pub fn spawn_check(ctx: egui::Context) -> Option<mpsc::Receiver<AvailableUpdate>> {
    let repo = resolve_releases_repo(option_env!("ZENHA_RELEASES_REPO"));
    let url = latest_release_url(repo);
    let current_version = env!("CARGO_PKG_VERSION");

    let (tx, rx) = mpsc::channel();
    match std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            if let Some(update) = fetch_latest_release(&url, repo, current_version)
                && tx.send(update).is_ok()
            {
                ctx.request_repaint();
            }
        }) {
        Ok(_) => Some(rx),
        Err(err) => {
            tracing::debug!(%err, "could not spawn update check thread");
            None
        }
    }
}

fn fetch_latest_release(url: &str, repo: &str, current_version: &str) -> Option<AvailableUpdate> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            tracing::debug!(%err, "could not initialize runtime for update check");
            return None;
        }
    };

    runtime.block_on(async {
        let client = match reqwest::Client::builder()
            .https_only(true)
            .timeout(REQUEST_TIMEOUT)
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                tracing::debug!(%err, "could not build update check http client");
                return None;
            }
        };

        let response = match client
            .get(url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                tracing::debug!(%err, "update check request failed");
                return None;
            }
        };

        let response = match response.error_for_status() {
            Ok(resp) => resp,
            Err(err) => {
                tracing::debug!(%err, "update check returned non-success status");
                return None;
            }
        };

        let release: GitHubRelease = match response.json().await {
            Ok(release) => release,
            Err(err) => {
                tracing::debug!(%err, "could not decode github release payload");
                return None;
            }
        };

        evaluate_release(
            current_version,
            repo,
            &release.tag_name,
            &release.html_url,
            release.prerelease,
            release.draft,
            &release.assets,
        )
    })
}
