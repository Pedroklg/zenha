//! Installer download with size limit, SHA-256 hash and restricted redirects.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use sha2::{Digest, Sha256};

use super::install::{installer_paths, update_temp_dir};
use super::{InstallerAsset, MAX_INSTALLER_BYTES, USER_AGENT};

const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(30);
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(50);
const MAX_REDIRECTS: usize = 10;

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

pub(super) fn is_allowed_download_redirect(url: &reqwest::Url) -> bool {
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
pub(super) fn download_to_paths_blocking(
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
