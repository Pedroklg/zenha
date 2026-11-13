//! Gravação segura de arquivos recebidos: limite de tamanho, arquivo parcial apagado e andamento espaçado.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
pub const MAX_CONCURRENT_RECEIVES: usize = 8;

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
                .join("zenha-recebidos")
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
}
