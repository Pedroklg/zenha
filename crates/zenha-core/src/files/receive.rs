//! Safe writing of received files: size limit, partial file deleted and spaced-out progress.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
pub const MAX_CONCURRENT_RECEIVES: usize = 8;
const RECEIVED_ROOT_DIR: &str = "zenha-recebidos";

#[derive(Debug, thiserror::Error)]
pub enum ReceiveError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("received more than the announced {expected} bytes")]
    Overflow { expected: u64 },
    #[error("received {received} of {expected} bytes")]
    Incomplete { expected: u64, received: u64 },
}

impl ReceiveError {
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::Io(_) => "não foi possível gravar o arquivo neste computador",
            Self::Overflow { .. } | Self::Incomplete { .. } => {
                "o arquivo recebido não bate com o tamanho informado"
            }
        }
    }
}

/// Lets an event through at most once per interval.
#[derive(Debug)]
pub struct Throttle {
    interval: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    pub fn ready(&mut self, now: Instant) -> bool {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < self.interval => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// Destination file of one incoming transfer; removed on drop unless `finish` succeeded.
#[derive(Debug)]
pub struct IncomingFile {
    writer: Option<BufWriter<File>>,
    path: PathBuf,
    expected: u64,
    received: u64,
    keep: bool,
}

impl IncomingFile {
    /// Never overwrites: an existing file (or link) at `path` is an error.
    pub fn create(path: &Path, expected: u64) -> Result<Self, ReceiveError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            writer: Some(BufWriter::new(file)),
            path: path.to_owned(),
            expected,
            received: 0,
            keep: false,
        })
    }

    /// Returns the total received so far.
    pub fn write_chunk(&mut self, chunk: &[u8]) -> Result<u64, ReceiveError> {
        let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        if self.received.saturating_add(len) > self.expected {
            return Err(ReceiveError::Overflow {
                expected: self.expected,
            });
        }
        let Some(writer) = self.writer.as_mut() else {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe).into());
        };
        writer.write_all(chunk)?;
        self.received += len;
        Ok(self.received)
    }

    pub fn finish(mut self) -> Result<PathBuf, ReceiveError> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        if self.received != self.expected {
            return Err(ReceiveError::Incomplete {
                expected: self.expected,
                received: self.received,
            });
        }
        self.keep = true;
        Ok(self.path.clone())
    }
}

impl Drop for IncomingFile {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        self.writer = None;
        if let Err(err) = fs::remove_file(&self.path) {
            tracing::debug!(%err, "could not remove partial file");
        }
    }
}

/// Where received files go and how many transfers may be running at once.
#[derive(Debug)]
pub struct FileReceiver {
    dir: PathBuf,
    active: Arc<AtomicUsize>,
}

impl FileReceiver {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            active: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn for_process() -> Self {
        Self::new(
            std::env::temp_dir()
                .join(RECEIVED_ROOT_DIR)
                .join(std::process::id().to_string()),
        )
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn try_acquire(&self) -> Option<ReceiveSlot> {
        let taken = self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_CONCURRENT_RECEIVES).then_some(n + 1)
            })
            .is_ok();
        taken.then(|| ReceiveSlot(self.active.clone()))
    }

    pub fn remove_dir(&self) {
        match fs::remove_dir_all(&self.dir) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => tracing::debug!(%err, "could not remove the received files folder"),
        }
    }
}

#[derive(Debug)]
pub struct ReceiveSlot(Arc<AtomicUsize>);

impl Drop for ReceiveSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Deletes temporary `zenha-recebidos/<pid>` folders left behind by exited processes.
pub fn cleanup_orphan_received_dirs() {
    let root = std::env::temp_dir().join(RECEIVED_ROOT_DIR);
    cleanup_orphan_dirs_in(&root, process_is_alive);
}

fn cleanup_orphan_dirs_in(root: &Path, is_alive: impl Fn(u32) -> bool) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::debug!(%err, "could not read received files root folder");
            return;
        }
    };
    let current_pid = std::process::id();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == current_pid || is_alive(pid) {
            continue;
        }
        if let Err(err) = fs::remove_dir_all(entry.path())
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::debug!(%err, pid, "could not remove orphan received files folder");
        }
    }
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "checking whether another process PID is still alive requires Win32 OpenProcess/GetExitCodeProcess"
)]
fn process_is_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::core::HRESULT;

    // SAFETY: `OpenProcess` with `PROCESS_QUERY_LIMITED_INFORMATION` is a read-only handle lookup
    // by PID; any returned handle is closed before returning.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => {
            let mut exit_code = 0u32;
            // SAFETY: `handle` is a valid process handle and `exit_code` is a live local.
            let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) }.is_ok();
            // SAFETY: `handle` was returned by `OpenProcess` and is closed once here.
            unsafe {
                let _ = CloseHandle(handle);
            }
            !ok || exit_code == STILL_ACTIVE.0 as u32
        }
        Err(err) => {
            // `ERROR_INVALID_PARAMETER` means no process with this PID exists;
            // `ERROR_ACCESS_DENIED` (or any other error) errs on the side of keeping it.
            err.code() != HRESULT::from_win32(ERROR_INVALID_PARAMETER.0)
        }
    }
}

#[cfg(not(windows))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("zenha-recv-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn throttle_spaces_events() {
        let start = Instant::now();
        let mut throttle = Throttle::new(PROGRESS_INTERVAL);
        assert_eq!(PROGRESS_INTERVAL, Duration::from_millis(100));
        assert!(throttle.ready(start));
        assert!(!throttle.ready(start + Duration::from_millis(50)));
        assert!(!throttle.ready(start + Duration::from_millis(99)));
        assert!(throttle.ready(start + Duration::from_millis(100)));
        assert!(!throttle.ready(start + Duration::from_millis(150)));
        assert!(throttle.ready(start + Duration::from_millis(300)));
    }

    #[test]
    fn complete_file_is_kept() {
        let dir = temp_dir();
        let path = dir.join("a.bin");
        let mut file = IncomingFile::create(&path, 6).unwrap();
        assert_eq!(file.write_chunk(b"abc").unwrap(), 3);
        assert_eq!(file.write_chunk(b"def").unwrap(), 6);
        assert_eq!(file.finish().unwrap(), path);
        assert_eq!(fs::read(&path).unwrap(), b"abcdef");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overflow_is_rejected_and_partial_file_removed() {
        let dir = temp_dir();
        let path = dir.join("a.bin");
        let mut file = IncomingFile::create(&path, 4).unwrap();
        file.write_chunk(b"abc").unwrap();
        assert!(matches!(
            file.write_chunk(b"de"),
            Err(ReceiveError::Overflow { expected: 4 })
        ));
        drop(file);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_file_fails_and_is_removed() {
        let dir = temp_dir();
        let path = dir.join("a.bin");
        let mut file = IncomingFile::create(&path, 10).unwrap();
        file.write_chunk(b"abc").unwrap();
        assert!(matches!(
            file.finish(),
            Err(ReceiveError::Incomplete {
                expected: 10,
                received: 3
            })
        ));
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_messages_do_not_leak_details() {
        let io = ReceiveError::Io(io::Error::other("C:\\secreto"));
        assert!(!io.user_message().contains("secreto"));
        let short = ReceiveError::Incomplete {
            expected: 2,
            received: 1,
        };
        assert_ne!(io.user_message(), short.user_message());
    }

    #[test]
    fn existing_file_is_never_overwritten() {
        let dir = temp_dir();
        let path = dir.join("a.bin");
        let first = IncomingFile::create(&path, 1).unwrap();
        assert!(IncomingFile::create(&path, 1).is_err());
        drop(first);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn slots_are_limited_and_released() {
        let receiver = FileReceiver::new(temp_dir());
        let slots: Vec<_> = (0..MAX_CONCURRENT_RECEIVES)
            .map(|_| receiver.try_acquire().unwrap())
            .collect();
        assert!(receiver.try_acquire().is_none());
        drop(slots);
        assert!(receiver.try_acquire().is_some());
    }

    #[test]
    fn remove_dir_deletes_the_folder_and_tolerates_absence() {
        let dir = temp_dir();
        let receiver = FileReceiver::new(dir.clone());
        receiver.remove_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x"), b"1").unwrap();
        receiver.remove_dir();
        assert!(!dir.exists());
    }

    #[test]
    fn process_folder_lives_under_the_temp_dir() {
        let receiver = FileReceiver::for_process();
        assert!(receiver.dir().starts_with(std::env::temp_dir()));
        assert!(receiver.dir().ends_with(std::process::id().to_string()));
    }

    #[test]
    fn cleanup_orphan_dirs_removes_dead_pids_and_keeps_live_and_non_numeric() {
        let root = temp_dir();
        let current = std::process::id();
        let dead_pid = if current == 100_001 { 100_002 } else { 100_001 };
        let live_pid = if current == 100_003 { 100_004 } else { 100_003 };

        let dead_dir = root.join(dead_pid.to_string());
        let live_dir = root.join(live_pid.to_string());
        let current_dir = root.join(current.to_string());
        let other_dir = root.join("not-a-pid");
        let numeric_file = root.join("99999");

        fs::create_dir_all(&dead_dir).unwrap();
        fs::write(dead_dir.join("stale.bin"), b"old").unwrap();
        fs::create_dir_all(&live_dir).unwrap();
        fs::write(live_dir.join("active.bin"), b"live").unwrap();
        fs::create_dir_all(&current_dir).unwrap();
        fs::create_dir_all(&other_dir).unwrap();
        fs::write(&numeric_file, b"file").unwrap();

        cleanup_orphan_dirs_in(&root, |pid| pid == live_pid);

        assert!(!dead_dir.exists());
        assert!(live_dir.exists());
        assert!(current_dir.exists());
        assert!(other_dir.exists());
        assert!(numeric_file.exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_orphan_dirs_ignores_missing_root() {
        let root = temp_dir();
        cleanup_orphan_dirs_in(&root, |_| false);
    }

    #[cfg(windows)]
    #[test]
    fn current_process_is_reported_alive() {
        assert!(process_is_alive(std::process::id()));
    }
}
