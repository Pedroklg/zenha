//! Checagem, download verificado e execução da atualização automática via GitHub Releases.

use std::fmt;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use directories::BaseDirs;
use eframe::egui;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const DEFAULT_RELEASES_REPO: &str = "Pedroklg/zenha";
pub const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;
pub const INSTALLER_ARGS: &[&str] = &[
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/CLOSEAPPLICATIONS",
];

const UPDATE_DIR_NAME: &str = "zenha-update";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(30);
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(50);
const MAX_REDIRECTS: usize = 10;
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDownloadEvent {
    Progress {
        id: u64,
        downloaded: u64,
        total: u64,
    },
    Finished {
        id: u64,
        result: Result<PathBuf, String>,
    },
}

pub struct DownloadHandle {
    id: u64,
    cancel: Arc<AtomicBool>,
}

impl DownloadHandle {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for DownloadHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadError {
    Cancelled,
    Network,
    HttpStatus(u16),
    TooLarge,
    SizeMismatch,
    HashMismatch,
    Io,
}

impl DownloadError {
    pub fn user_message(self) -> &'static str {
        match self {
            Self::Cancelled => "Atualização cancelada.",
            Self::Network => "Falha de rede ao baixar atualização.",
            Self::HttpStatus(_) => "Erro ao baixar atualização.",
            Self::TooLarge => "Arquivo passa do limite de 200 MB.",
            Self::SizeMismatch => "Tamanho do arquivo não confere.",
            Self::HashMismatch => "Hash do arquivo não confere.",
            Self::Io => "Erro ao salvar arquivo temporário.",
        }
    }
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

pub fn parse_semver(raw: &str) -> Option<SemVer> {
    let trimmed = raw.trim();
    let without_prefix = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);

    // Build metadata (+...) is ignored; prereleases (-...) are rejected.
    let core = without_prefix.split('+').next()?;
    if core.contains('-') {
        return None;
    }
    let mut parts = core.split('.');
    let major = parse_component(parts.next()?)?;
    let minor = parse_component(parts.next()?)?;
    let patch = parse_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }

    Some(SemVer {
        major,
        minor,
        patch,
    })
}

fn parse_component(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse::<u64>().ok()
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

pub fn should_show_update(update: &AvailableUpdate, dismissed_version: Option<&str>) -> bool {
    let Some(latest) = parse_semver(&update.version) else {
        return false;
    };
    let Some(dismissed) = dismissed_version.and_then(parse_semver) else {
        return true;
    };
    latest > dismissed
}

pub fn normalize_windows_path(raw: &str) -> Option<String> {
    let replaced = raw.trim().replace('/', "\\");
    if replaced.is_empty() {
        return None;
    }
    let without_verbatim = if let Some(unc_rest) = replaced.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc_rest}")
    } else if let Some(rest) = replaced.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else if let Some(rest) = replaced.strip_prefix(r"\\.\") {
        rest.to_owned()
    } else {
        replaced
    };

    let (root, rest) = if let Some(unc) = without_verbatim.strip_prefix(r"\\") {
        let mut parts = unc.split('\\').filter(|s| !s.is_empty());
        let server = parts.next()?;
        let share = parts.next()?;
        let prefix = format!(r"\\{}\{}", server.to_lowercase(), share.to_lowercase());
        let remaining: Vec<&str> = parts.collect();
        (prefix, remaining)
    } else {
        let mut parts = without_verbatim.split('\\');
        let first = parts.next()?;
        if first.len() == 2 && first.as_bytes()[0].is_ascii_alphabetic() && first.ends_with(':') {
            (
                first.to_ascii_lowercase(),
                parts.filter(|s| !s.is_empty()).collect(),
            )
        } else {
            return None;
        }
    };

    let mut stack: Vec<String> = Vec::new();
    for segment in rest {
        match segment {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other.to_lowercase()),
        }
    }

    if stack.is_empty() {
        Some(format!(r"{root}\"))
    } else {
        Some(format!(r"{root}\{}", stack.join(r"\")))
    }
}

pub fn is_installed_exe_path(current_exe: &Path, local_app_data: &Path) -> bool {
    let Some(exe_str) = current_exe.to_str() else {
        return false;
    };
    let Some(local_str) = local_app_data.to_str() else {
        return false;
    };
    let Some(normalized_exe) = normalize_windows_path(exe_str) else {
        return false;
    };
    let expected_raw = format!(r"{local_str}\Programs\Zenha\zenha.exe");
    let Some(normalized_expected) = normalize_windows_path(&expected_raw) else {
        return false;
    };
    normalized_exe == normalized_expected
}

pub fn is_installed_exe() -> bool {
    #[cfg(not(windows))]
    {
        false
    }
    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let Some(local_app_data) = local_app_data_dir() else {
            return false;
        };
        if is_installed_exe_path(&exe, &local_app_data) {
            return true;
        }
        let canon_exe = fs::canonicalize(&exe).unwrap_or(exe);
        let canon_local = fs::canonicalize(&local_app_data).unwrap_or(local_app_data);
        is_installed_exe_path(&canon_exe, &canon_local)
    }
}

#[cfg(windows)]
fn local_app_data_dir() -> Option<PathBuf> {
    BaseDirs::new()
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .filter(|val| !val.is_empty())
                .map(PathBuf::from)
        })
}

pub fn update_temp_dir() -> PathBuf {
    std::env::temp_dir().join(UPDATE_DIR_NAME)
}

pub fn installer_paths(dir: &Path, version: &str) -> Option<(PathBuf, PathBuf)> {
    let parsed = parse_semver(version)?;
    let exe_name = format!("Zenha-{parsed}-setup.exe");
    let part_name = format!("{exe_name}.part");
    Some((dir.join(part_name), dir.join(exe_name)))
}

pub fn cleanup_update_dir(dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::debug!(%err, path = %dir.display(), "could not list update temp directory");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && let Err(err) = fs::remove_file(&path)
        {
            tracing::debug!(%err, path = %path.display(), "could not remove stale update file");
        }
    }

    let _ = fs::remove_dir(dir);
}

pub fn next_demo_version(current_version: &str) -> String {
    match parse_semver(current_version) {
        Some(ver) => SemVer {
            major: ver.major,
            minor: ver.minor.saturating_add(1),
            patch: 0,
        }
        .to_string(),
        None => "0.2.0".to_owned(),
    }
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

pub fn spawn_download(
    ctx: egui::Context,
    id: u64,
    version: String,
    asset: InstallerAsset,
    tx: mpsc::Sender<UpdateDownloadEvent>,
) -> Option<DownloadHandle> {
    let Some((part_path, final_path)) = installer_paths(&update_temp_dir(), &version) else {
        let _ = tx.send(UpdateDownloadEvent::Finished {
            id,
            result: Err(DownloadError::Io.user_message().to_owned()),
        });
        ctx.request_repaint();
        return None;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let worker_tx = tx.clone();
    let worker_ctx = ctx.clone();

    let spawned = std::thread::Builder::new()
        .name("update-download".into())
        .spawn(move || {
            let mut last_emit = Instant::now()
                .checked_sub(PROGRESS_EMIT_INTERVAL)
                .unwrap_or_else(Instant::now);
            let result = download_to_paths_blocking(
                &asset.url,
                asset.size,
                asset.sha256,
                &part_path,
                &final_path,
                &worker_cancel,
                true,
                |downloaded, total| {
                    let now = Instant::now();
                    if downloaded == total
                        || now.saturating_duration_since(last_emit) >= PROGRESS_EMIT_INTERVAL
                    {
                        last_emit = now;
                        if worker_tx
                            .send(UpdateDownloadEvent::Progress {
                                id,
                                downloaded,
                                total,
                            })
                            .is_ok()
                        {
                            worker_ctx.request_repaint();
                        }
                    }
                },
            );

            let message_result = match result {
                Ok(path) => Ok(path),
                Err(DownloadError::Cancelled) => return,
                Err(err) => Err(err.user_message().to_owned()),
            };
            if worker_tx
                .send(UpdateDownloadEvent::Finished {
                    id,
                    result: message_result,
                })
                .is_ok()
            {
                worker_ctx.request_repaint();
            }
        });

    match spawned {
        Ok(_) => Some(DownloadHandle { id, cancel }),
        Err(err) => {
            tracing::warn!(%err, "could not start update download thread");
            let _ = tx.send(UpdateDownloadEvent::Finished {
                id,
                result: Err(DownloadError::Io.user_message().to_owned()),
            });
            ctx.request_repaint();
            None
        }
    }
}

pub fn launch_installer(path: &Path) -> std::io::Result<()> {
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "installer path is not a file",
        ));
    }
    #[cfg(not(windows))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "installer execution is only supported on windows",
        ))
    }
    #[cfg(windows)]
    {
        let target = std::path::absolute(path)?;
        Command::new(&target)
            .args(INSTALLER_ARGS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }
}

struct PartGuard {
    path: PathBuf,
    armed: bool,
}

impl PartGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct PartVerifier<W: Write> {
    writer: W,
    hasher: Sha256,
    written: u64,
    expected_size: u64,
    expected_sha256: [u8; 32],
}

impl<W: Write> PartVerifier<W> {
    pub fn new(
        writer: W,
        expected_size: u64,
        expected_sha256: [u8; 32],
    ) -> Result<Self, DownloadError> {
        if expected_size == 0 {
            return Err(DownloadError::SizeMismatch);
        }
        if expected_size > MAX_INSTALLER_BYTES {
            return Err(DownloadError::TooLarge);
        }
        Ok(Self {
            writer,
            hasher: Sha256::new(),
            written: 0,
            expected_size,
            expected_sha256,
        })
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<u64, DownloadError> {
        let chunk_len = u64::try_from(chunk.len()).map_err(|_| DownloadError::TooLarge)?;
        let next = self
            .written
            .checked_add(chunk_len)
            .ok_or(DownloadError::TooLarge)?;
        if next > MAX_INSTALLER_BYTES {
            return Err(DownloadError::TooLarge);
        }
        if next > self.expected_size {
            return Err(DownloadError::SizeMismatch);
        }
        self.writer
            .write_all(chunk)
            .map_err(|_| DownloadError::Io)?;
        self.hasher.update(chunk);
        self.written = next;
        Ok(next)
    }

    pub fn finish(mut self) -> Result<(), DownloadError> {
        self.writer.flush().map_err(|_| DownloadError::Io)?;
        if self.written != self.expected_size {
            return Err(DownloadError::SizeMismatch);
        }
        let digest: [u8; 32] = self.hasher.finalize().into();
        if digest != self.expected_sha256 {
            return Err(DownloadError::HashMismatch);
        }
        Ok(())
    }
}

fn is_allowed_download_redirect(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    url.host_str().is_some_and(is_allowed_download_host)
}

fn is_allowed_download_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower == "github.com"
        || lower == "githubusercontent.com"
        || lower.ends_with(".githubusercontent.com")
}

#[expect(
    clippy::too_many_arguments,
    reason = "download helper takes verification parameters and output paths explicitly"
)]
fn download_to_paths_blocking(
    url: &str,
    expected_size: u64,
    expected_sha256: [u8; 32],
    part_path: &Path,
    final_path: &Path,
    cancel: &AtomicBool,
    https_only: bool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<PathBuf, DownloadError> {
    if expected_size == 0 {
        return Err(DownloadError::SizeMismatch);
    }
    if expected_size > MAX_INSTALLER_BYTES {
        return Err(DownloadError::TooLarge);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(DownloadError::Cancelled);
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            tracing::warn!(%err, "could not initialize runtime for update download");
            DownloadError::Io
        })?;

    runtime.block_on(download_and_verify(
        url,
        expected_size,
        expected_sha256,
        part_path,
        final_path,
        cancel,
        https_only,
        &mut on_progress,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "async downloader separates pure verification params from paths and callbacks"
)]
async fn download_and_verify(
    url: &str,
    expected_size: u64,
    expected_sha256: [u8; 32],
    part_path: &Path,
    final_path: &Path,
    cancel: &AtomicBool,
    https_only: bool,
    on_progress: &mut impl FnMut(u64, u64),
) -> Result<PathBuf, DownloadError> {
    let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if !is_allowed_download_redirect(attempt.url()) {
            attempt.error("disallowed redirect host")
        } else {
            attempt.follow()
        }
    });

    let client = reqwest::Client::builder()
        .https_only(https_only)
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .read_timeout(DOWNLOAD_READ_TIMEOUT)
        .redirect(redirect_policy)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|err| {
            tracing::warn!(%err, "could not build update download client");
            DownloadError::Network
        })?;

    let response = client.get(url).send().await.map_err(|err| {
        tracing::warn!(%err, "update download request failed");
        DownloadError::Network
    })?;

    let status = response.status();
    let mut response = response.error_for_status().map_err(|_| {
        tracing::warn!(
            status = status.as_u16(),
            "update download returned error status"
        );
        DownloadError::HttpStatus(status.as_u16())
    })?;

    if let Some(content_length) = response.content_length() {
        if content_length > MAX_INSTALLER_BYTES {
            return Err(DownloadError::TooLarge);
        }
        if content_length != expected_size {
            return Err(DownloadError::SizeMismatch);
        }
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(DownloadError::Cancelled);
    }

    if let Some(parent) = part_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            tracing::warn!(%err, path = %parent.display(), "could not create update directory");
            DownloadError::Io
        })?;
    }
    let _ = fs::remove_file(part_path);
    let _ = fs::remove_file(final_path);

    // Declare guard before opening the file so the Windows handle closes before Drop deletes `.part`.
    let mut guard = PartGuard::new(part_path.to_path_buf());
    {
        let file = File::create(part_path).map_err(|err| {
            tracing::warn!(%err, path = %part_path.display(), "could not create update part file");
            DownloadError::Io
        })?;
        let mut verifier = PartVerifier::new(BufWriter::new(file), expected_size, expected_sha256)?;

        while let Some(chunk) = response.chunk().await.map_err(|err| {
            tracing::warn!(%err, "update download stream failed");
            DownloadError::Network
        })? {
            if cancel.load(Ordering::Relaxed) {
                return Err(DownloadError::Cancelled);
            }
            let downloaded = verifier.push_chunk(&chunk)?;
            on_progress(downloaded, expected_size);
        }

        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        verifier.finish()?;
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(DownloadError::Cancelled);
    }

    let _ = fs::remove_file(final_path);
    fs::rename(part_path, final_path).map_err(|err| {
        tracing::warn!(%err, "could not rename verified installer file");
        DownloadError::Io
    })?;
    guard.disarm();
    Ok(final_path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicU64;

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn isolated_test_dir(label: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "zenha-update-test-{label}-{}-{id}",
            std::process::id()
        ))
    }

    fn sha256_of(data: &[u8]) -> ([u8; 32], String) {
        let digest: [u8; 32] = Sha256::digest(data).into();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        (digest, format!("sha256:{hex}"))
    }

    fn sample_asset(name: &str, size: u64, digest: Option<&str>, url: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            size,
            digest: digest.map(str::to_owned),
            browser_download_url: url.to_owned(),
        }
    }

    fn spawn_http_server(
        chunks: Vec<Vec<u8>>,
        declared_len: Option<u64>,
        delay_per_chunk: Duration,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut req_buf = [0u8; 1024];
            let _ = stream.read(&mut req_buf);
            let total: usize = chunks.iter().map(Vec::len).sum();
            let content_len = declared_len.unwrap_or(total as u64);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {content_len}\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(header.as_bytes()).is_err() {
                return;
            }
            for chunk in chunks {
                if !delay_per_chunk.is_zero() {
                    std::thread::sleep(delay_per_chunk);
                }
                if stream.write_all(&chunk).is_err() {
                    return;
                }
                let _ = stream.flush();
            }
        });
        (
            format!("http://{addr}/Zenha-0.2.0-windows-x64-setup.exe"),
            handle,
        )
    }

    #[test]
    fn parses_plain_and_v_prefixed_versions() {
        assert_eq!(
            parse_semver("0.1.0"),
            Some(SemVer {
                major: 0,
                minor: 1,
                patch: 0
            })
        );
        assert_eq!(
            parse_semver("v1.2.3"),
            Some(SemVer {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        assert_eq!(
            parse_semver(" V10.20.30+build1 "),
            Some(SemVer {
                major: 10,
                minor: 20,
                patch: 30
            })
        );
    }

    #[test]
    fn ignores_prereleases_and_malformed_versions() {
        assert_eq!(parse_semver("v0.2.0-rc.1"), None);
        assert_eq!(parse_semver("0.2.0-alpha"), None);
        assert_eq!(parse_semver("1.0.0-beta+12"), None);
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("v1.2"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("+1.0.0"), None);
        assert_eq!(parse_semver("1.two.3"), None);
    }

    #[test]
    fn semver_ordering_compares_numerically() {
        let v0_2_0 = parse_semver("0.2.0");
        let v0_10_0 = parse_semver("0.10.0");
        assert!(v0_10_0 > v0_2_0);
    }

    #[test]
    fn evaluate_release_accepts_only_newer_stable_releases() {
        let update = evaluate_release(
            "0.1.0",
            DEFAULT_RELEASES_REPO,
            "v0.2.0",
            "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
            false,
            false,
            &[],
        );
        assert_eq!(
            update,
            Some(AvailableUpdate {
                version: "0.2.0".to_owned(),
                url: "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0".to_owned(),
                installer: None,
            })
        );

        assert_eq!(
            evaluate_release(
                "0.2.0",
                DEFAULT_RELEASES_REPO,
                "v0.2.0",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
                false,
                false,
                &[],
            ),
            None
        );
        assert_eq!(
            evaluate_release(
                "0.3.0",
                DEFAULT_RELEASES_REPO,
                "v0.2.9",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.9",
                false,
                false,
                &[],
            ),
            None
        );
    }

    #[test]
    fn evaluate_release_ignores_prereleases_drafts_and_unsafe_urls() {
        assert_eq!(
            evaluate_release(
                "0.1.0",
                DEFAULT_RELEASES_REPO,
                "v0.2.0",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
                true,
                false,
                &[],
            ),
            None
        );
        assert_eq!(
            evaluate_release(
                "0.1.0",
                DEFAULT_RELEASES_REPO,
                "v0.2.0",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
                false,
                true,
                &[],
            ),
            None
        );
        assert_eq!(
            evaluate_release(
                "0.1.0",
                DEFAULT_RELEASES_REPO,
                "v0.2.0-rc.1",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0-rc.1",
                false,
                false,
                &[],
            ),
            None
        );
        assert_eq!(
            evaluate_release(
                "0.1.0",
                DEFAULT_RELEASES_REPO,
                "v0.2.0",
                "file:///etc/passwd",
                false,
                false,
                &[],
            ),
            None
        );
    }

    #[test]
    fn should_show_update_respects_dismissed_version_until_newer_arrives() {
        let v0_2_0 = AvailableUpdate {
            version: "0.2.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0".to_owned(),
            installer: None,
        };
        let v0_3_0 = AvailableUpdate {
            version: "0.3.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.3.0".to_owned(),
            installer: None,
        };

        assert!(should_show_update(&v0_2_0, None));
        assert!(!should_show_update(&v0_2_0, Some("0.2.0")));
        assert!(!should_show_update(&v0_2_0, Some("v0.2.0")));
        assert!(!should_show_update(&v0_2_0, Some("0.2.5")));
        assert!(should_show_update(&v0_3_0, Some("0.2.0")));
        assert!(should_show_update(&v0_2_0, Some("corrupted")));
    }

    #[test]
    fn resolves_releases_repo_with_default_fallback() {
        assert_eq!(resolve_releases_repo(None), "Pedroklg/zenha");
        assert_eq!(resolve_releases_repo(Some("")), "Pedroklg/zenha");
        assert_eq!(resolve_releases_repo(Some("  ")), "Pedroklg/zenha");
        assert_eq!(resolve_releases_repo(Some(" org/custom ")), "org/custom");
        assert_eq!(
            latest_release_url("org/custom"),
            "https://api.github.com/repos/org/custom/releases/latest"
        );
    }

    #[test]
    fn parse_sha256_digest_validates_prefix_algorithm_and_hex() {
        let (bytes, valid) = sha256_of(b"zenha installer");
        assert_eq!(parse_sha256_digest(&valid), Some(bytes));
        assert_eq!(
            parse_sha256_digest(&valid.to_uppercase().replace("SHA256:", "sha256:")),
            Some(bytes)
        );
        assert_eq!(parse_sha256_digest(&format!("  {valid}  ")), Some(bytes));

        assert_eq!(parse_sha256_digest(""), None);
        assert_eq!(parse_sha256_digest("SHA256:0000"), None);
        assert_eq!(
            parse_sha256_digest(&valid.replace("sha256:", "sha512:")),
            None
        );
        assert_eq!(parse_sha256_digest(&valid[7..]), None);
        assert_eq!(parse_sha256_digest(&format!("{valid}00")), None);
        assert_eq!(parse_sha256_digest(&valid[..valid.len() - 2]), None);
        let non_hex = format!("sha256:{}zz", &valid[7..valid.len() - 2]);
        assert_eq!(parse_sha256_digest(&non_hex), None);
    }

    #[test]
    fn select_installer_asset_validates_name_url_size_and_digest() {
        let (hash, digest) = sha256_of(b"setup-binary");
        let valid_url = "https://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe";
        let zip_asset = sample_asset(
            "Zenha-0.2.0-windows-x64.zip",
            100,
            Some(&digest),
            "https://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64.zip",
        );
        let valid_setup = sample_asset(
            "Zenha-0.2.0-windows-x64-setup.exe",
            12_345,
            Some(&digest),
            valid_url,
        );

        let update = evaluate_release(
            "0.1.5",
            DEFAULT_RELEASES_REPO,
            "v0.2.0",
            "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
            false,
            false,
            &[zip_asset.clone(), valid_setup.clone()],
        )
        .unwrap();
        assert_eq!(
            update.installer,
            Some(InstallerAsset {
                url: valid_url.to_owned(),
                size: 12_345,
                sha256: hash,
            })
        );

        // Missing installer asset falls back to link-only update.
        assert_eq!(
            select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[zip_asset]),
            None
        );

        // Wrong version in installer name is rejected.
        let wrong_ver = sample_asset(
            "Zenha-0.2.1-windows-x64-setup.exe",
            100,
            Some(&digest),
            valid_url,
        );
        assert_eq!(
            select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[wrong_ver]),
            None
        );

        // Missing or invalid digest falls back to link-only.
        let no_digest = sample_asset("Zenha-0.2.0-windows-x64-setup.exe", 100, None, valid_url);
        assert_eq!(
            select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[no_digest]),
            None
        );
        let bad_algo = sample_asset(
            "Zenha-0.2.0-windows-x64-setup.exe",
            100,
            Some("sha512:abcd"),
            valid_url,
        );
        assert_eq!(
            select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[bad_algo]),
            None
        );

        // Size zero or above 200 MB is rejected.
        let zero_size = sample_asset(
            "Zenha-0.2.0-windows-x64-setup.exe",
            0,
            Some(&digest),
            valid_url,
        );
        assert_eq!(
            select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[zero_size]),
            None
        );
        let oversized = sample_asset(
            "Zenha-0.2.0-windows-x64-setup.exe",
            MAX_INSTALLER_BYTES + 1,
            Some(&digest),
            valid_url,
        );
        assert_eq!(
            select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[oversized]),
            None
        );
        let max_exact = sample_asset(
            "Zenha-0.2.0-windows-x64-setup.exe",
            MAX_INSTALLER_BYTES,
            Some(&digest),
            valid_url,
        );
        assert!(select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[max_exact]).is_some());

        // URL outside repository is rejected.
        let foreign_repo = sample_asset(
            "Zenha-0.2.0-windows-x64-setup.exe",
            100,
            Some(&digest),
            "https://github.com/other/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe",
        );
        assert_eq!(
            select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[foreign_repo]),
            None
        );
    }

    #[test]
    fn repo_download_url_and_redirect_validation_block_untrusted_targets() {
        assert!(is_safe_repo_download_url(
            "https://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe",
            "Pedroklg/zenha",
        ));
        assert!(is_safe_repo_download_url(
            "https://github.com/pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe",
            "Pedroklg/zenha",
        ));
        assert!(!is_safe_repo_download_url(
            "http://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe",
            "Pedroklg/zenha",
        ));
        assert!(!is_safe_repo_download_url(
            "https://github.com/Pedroklg/zenha-evil/releases/download/v0.2.0/setup.exe",
            "Pedroklg/zenha",
        ));
        assert!(!is_safe_repo_download_url(
            "https://github.com.evil.com/Pedroklg/zenha/releases/download/v0.2.0/setup.exe",
            "Pedroklg/zenha",
        ));
        assert!(!is_safe_repo_download_url(
            "https://user@github.com/Pedroklg/zenha/releases/download/v0.2.0/setup.exe",
            "Pedroklg/zenha",
        ));
        assert!(!is_safe_repo_download_url(
            "https://github.com/Pedroklg/zenha/../../other/repo/setup.exe",
            "Pedroklg/zenha",
        ));
        assert!(!is_safe_repo_download_url(
            "https://github.com/Pedroklg/zenha/",
            "Pedroklg/zenha",
        ));

        assert!(is_allowed_download_redirect(
            &reqwest::Url::parse("https://objects.githubusercontent.com/abc/setup.exe").unwrap()
        ));
        assert!(is_allowed_download_redirect(
            &reqwest::Url::parse("https://release-assets.githubusercontent.com/setup.exe").unwrap()
        ));
        assert!(!is_allowed_download_redirect(
            &reqwest::Url::parse("http://objects.githubusercontent.com/abc/setup.exe").unwrap()
        ));
        assert!(!is_allowed_download_redirect(
            &reqwest::Url::parse("https://evilgithubusercontent.com/setup.exe").unwrap()
        ));
        assert!(!is_allowed_download_redirect(
            &reqwest::Url::parse("https://example.com/setup.exe").unwrap()
        ));
    }

    #[test]
    fn installed_exe_detection_compares_normalized_paths_case_insensitively() {
        let local = Path::new(r"C:\Users\Maria\AppData\Local");
        assert!(is_installed_exe_path(
            Path::new(r"C:\Users\Maria\AppData\Local\Programs\Zenha\zenha.exe"),
            local,
        ));
        assert!(is_installed_exe_path(
            Path::new(r"\\?\c:\users\MARIA\appdata\local\programs\zenha\ZENHA.EXE"),
            Path::new(r"C:/Users/Maria/AppData/Local/"),
        ));
        assert!(is_installed_exe_path(
            Path::new(r"C:\Users\Maria\AppData\Local\Programs\Zenha\.\sub\..\zenha.exe"),
            local,
        ));

        assert!(!is_installed_exe_path(
            Path::new(r"C:\Users\Maria\Downloads\Zenha\zenha.exe"),
            local,
        ));
        assert!(!is_installed_exe_path(
            Path::new(r"F:\cargo-target\zenha\debug\zenha.exe"),
            local,
        ));
        assert!(!is_installed_exe_path(
            Path::new(r"C:\Users\Maria\AppData\Local\Programs\Zenha\sub\zenha.exe"),
            local,
        ));
        assert!(!is_installed_exe_path(
            Path::new(r"C:\Users\Maria\AppData\Local\Programs\Zenha\zenha-portable.exe"),
            local,
        ));
        assert!(!is_installed_exe_path(Path::new(r"zenha.exe"), local));
    }

    #[test]
    fn part_verifier_verifies_chunks_size_and_sha256() {
        let payload = b"installer-payload-chunk-data";
        let (hash, _) = sha256_of(payload);

        let mut sink = Vec::new();
        let mut verifier = PartVerifier::new(&mut sink, payload.len() as u64, hash).unwrap();
        assert_eq!(verifier.push_chunk(&payload[..10]), Ok(10));
        assert_eq!(
            verifier.push_chunk(&payload[10..]),
            Ok(payload.len() as u64)
        );
        assert_eq!(verifier.finish(), Ok(()));
        assert_eq!(sink, payload);

        let mut wrong_hash = hash;
        wrong_hash[0] ^= 0xff;
        let mut sink = Vec::new();
        let mut verifier = PartVerifier::new(&mut sink, payload.len() as u64, wrong_hash).unwrap();
        verifier.push_chunk(payload).unwrap();
        assert_eq!(verifier.finish(), Err(DownloadError::HashMismatch));

        let mut sink = Vec::new();
        let mut verifier = PartVerifier::new(&mut sink, payload.len() as u64 + 1, hash).unwrap();
        verifier.push_chunk(payload).unwrap();
        assert_eq!(verifier.finish(), Err(DownloadError::SizeMismatch));

        let mut sink = Vec::new();
        let mut verifier = PartVerifier::new(&mut sink, 5, hash).unwrap();
        assert_eq!(
            verifier.push_chunk(b"123456"),
            Err(DownloadError::SizeMismatch)
        );

        let mut sink = Vec::new();
        assert!(matches!(
            PartVerifier::new(&mut sink, MAX_INSTALLER_BYTES + 1, hash),
            Err(DownloadError::TooLarge)
        ));
    }

    #[test]
    fn download_to_paths_streams_verifies_and_cleans_part_files() {
        let dir = isolated_test_dir("download");
        let (part_path, final_path) = installer_paths(&dir, "0.2.0").unwrap();
        let payload = b"MZ-fake-zenha-installer-payload-0123456789";
        let (hash, _) = sha256_of(payload);

        // 1. Valid multi-chunk download renames .part -> .exe and reports progress.
        let (url, server) = spawn_http_server(
            vec![payload[..15].to_vec(), payload[15..].to_vec()],
            None,
            Duration::ZERO,
        );
        let cancel = AtomicBool::new(false);
        let mut progress_steps = Vec::new();
        let out = download_to_paths_blocking(
            &url,
            payload.len() as u64,
            hash,
            &part_path,
            &final_path,
            &cancel,
            false,
            |downloaded, total| progress_steps.push((downloaded, total)),
        );
        let _ = server.join();
        assert_eq!(out, Ok(final_path.clone()));
        assert!(!part_path.exists());
        assert_eq!(fs::read(&final_path).unwrap(), payload);
        assert_eq!(
            progress_steps.last().copied(),
            Some((payload.len() as u64, payload.len() as u64))
        );

        // 2. Hash mismatch deletes .part and removes any prior final exe.
        let mut bad_hash = hash;
        bad_hash[0] ^= 1;
        let (url, server) = spawn_http_server(vec![payload.to_vec()], None, Duration::ZERO);
        let out = download_to_paths_blocking(
            &url,
            payload.len() as u64,
            bad_hash,
            &part_path,
            &final_path,
            &cancel,
            false,
            |_, _| {},
        );
        let _ = server.join();
        assert_eq!(out, Err(DownloadError::HashMismatch));
        assert!(!part_path.exists());
        assert!(!final_path.exists());

        // 3. Content-Length mismatch deletes .part.
        let (url, server) = spawn_http_server(
            vec![payload.to_vec()],
            Some(payload.len() as u64 + 5),
            Duration::ZERO,
        );
        let out = download_to_paths_blocking(
            &url,
            payload.len() as u64,
            hash,
            &part_path,
            &final_path,
            &cancel,
            false,
            |_, _| {},
        );
        let _ = server.join();
        assert_eq!(out, Err(DownloadError::SizeMismatch));
        assert!(!part_path.exists());
        assert!(!final_path.exists());

        // 4. Cancellation mid-stream deletes .part.
        let cancel_mid = Arc::new(AtomicBool::new(false));
        let cancel_flag = Arc::clone(&cancel_mid);
        let (url, server) = spawn_http_server(
            vec![payload[..10].to_vec(), payload[10..].to_vec()],
            None,
            Duration::from_millis(30),
        );
        let out = download_to_paths_blocking(
            &url,
            payload.len() as u64,
            hash,
            &part_path,
            &final_path,
            &cancel_mid,
            false,
            |_, _| cancel_flag.store(true, Ordering::Relaxed),
        );
        let _ = server.join();
        assert_eq!(out, Err(DownloadError::Cancelled));
        assert!(!part_path.exists());
        assert!(!final_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_update_dir_removes_leftovers_and_handles_missing_dir() {
        let dir = isolated_test_dir("cleanup");
        cleanup_update_dir(&dir);

        fs::create_dir_all(&dir).unwrap();
        let (part, exe) = installer_paths(&dir, "0.2.0").unwrap();
        fs::write(&part, b"partial").unwrap();
        fs::write(&exe, b"old-setup").unwrap();

        cleanup_update_dir(&dir);
        assert!(!part.exists());
        assert!(!exe.exists());
        assert!(!dir.exists());
    }

    #[test]
    fn next_demo_version_bumps_minor_component() {
        assert_eq!(next_demo_version("0.1.5"), "0.2.0");
        assert_eq!(next_demo_version("1.4.2"), "1.5.0");
        assert_eq!(next_demo_version("invalid"), "0.2.0");
    }

    #[test]
    fn launch_installer_rejects_missing_and_directory_paths() {
        let missing = isolated_test_dir("missing-installer").join("setup.exe");
        assert!(launch_installer(&missing).is_err());
        assert!(launch_installer(&std::env::temp_dir()).is_err());
        assert_eq!(
            INSTALLER_ARGS,
            &[
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/CLOSEAPPLICATIONS"
            ]
        );
    }
}
