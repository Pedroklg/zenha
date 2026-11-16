//! Validação e disparo de envio de arquivos na sessão.

use std::path::PathBuf;

use rooms_protocol::MAX_FILE_BYTES;

use crate::api::{Event, FileTransfer, SessionError, TransferState};
use crate::clock::unix_ms;
use crate::files::{mime_for_name, sanitize_file_name};

use super::Session;
use super::connection::Phase;

impl Session {
    pub(super) async fn send_file(&mut self, path: PathBuf) {
        if !matches!(self.phase, Phase::Connected { .. }) {
            self.emit(Event::CommandFailed(SessionError::NotInRoom));
            return;
        }
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                self.emit(Event::CommandFailed(SessionError::InvalidInput(
                    "isso não é um arquivo".to_owned(),
                )));
                return;
            }
            Err(err) => {
                tracing::debug!(%err, "could not read file metadata");
                self.emit(Event::CommandFailed(SessionError::InvalidInput(
                    "não foi possível abrir o arquivo".to_owned(),
                )));
                return;
            }
        };
        let size = metadata.len();
        if size == 0 {
            self.emit(Event::CommandFailed(SessionError::InvalidInput(
                "o arquivo está vazio".to_owned(),
            )));
            return;
        }
        if size > MAX_FILE_BYTES {
            self.emit(Event::CommandFailed(SessionError::InvalidInput(
                "o arquivo passa do limite de 50 MB".to_owned(),
            )));
            return;
        }
        let raw_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = sanitize_file_name(&raw_name);
        let mime = mime_for_name(&name).to_owned();
        let transfer = FileTransfer {
            id: uuid::Uuid::new_v4().to_string(),
            sender: self.config.display_name.clone(),
            name,
            size,
            mime,
            outgoing: true,
            sent_at_unix_ms: unix_ms(),
            state: TransferState::InProgress { bytes: 0 },
        };
        self.emit(Event::FileTransfer(transfer.clone()));
        if let Err(err) = self.backend.send_file(transfer.clone(), path).await {
            self.emit(Event::FileTransfer(FileTransfer {
                state: TransferState::Failed(err.to_string()),
                ..transfer
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rooms_protocol::MAX_FILE_BYTES;

    use crate::api::{Command, Event, FileTransfer, SessionError, TransferState};
    use crate::backend::BackendEvent;
    use crate::testing::BackendCall;

    use super::super::test_support::{Harness, harness};

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, len: u64) -> Self {
            let dir = std::env::temp_dir().join(format!("zenha-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            let file = std::fs::File::create(&path).unwrap();
            file.set_len(len).unwrap();
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    fn send_file_calls(h: &Harness) -> Vec<(FileTransfer, PathBuf)> {
        h.backend
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                BackendCall::SendFile { transfer, path } => Some((transfer, path)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn valid_file_starts_transfer_and_calls_backend() {
        let mut h = harness();
        h.join("geral").await;
        let file = TempFile::new("foto.PNG", 1234);
        h.send(Command::SendFile {
            path: file.0.clone(),
        });
        let Event::FileTransfer(initial) = h.next().await else {
            panic!("expected transfer");
        };
        assert_eq!(initial.name, "foto.PNG");
        assert_eq!(initial.mime, "image/png");
        assert_eq!(initial.size, 1234);
        assert_eq!(initial.sender, "pedro");
        assert!(initial.outgoing);
        assert!(initial.sent_at_unix_ms > 0);
        assert_eq!(initial.state, TransferState::InProgress { bytes: 0 });
        let calls = send_file_calls(&h);
        assert_eq!(calls, vec![(initial, file.0.clone())]);
    }

    #[tokio::test(start_paused = true)]
    async fn backend_failure_on_start_marks_transfer_failed() {
        let mut h = harness();
        h.join("geral").await;
        h.backend
            .fail_next_send_file(SessionError::Connection("caiu".to_owned()));
        let file = TempFile::new("a.bin", 10);
        h.send(Command::SendFile {
            path: file.0.clone(),
        });
        let Event::FileTransfer(first) = h.next().await else {
            panic!("expected transfer");
        };
        let Event::FileTransfer(second) = h.next().await else {
            panic!("expected failed transfer");
        };
        assert_eq!(first.id, second.id);
        assert!(matches!(second.state, TransferState::Failed(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn rejected_files_never_reach_backend() {
        let mut h = harness();
        h.join("geral").await;
        let big = TempFile::new("grande.bin", MAX_FILE_BYTES + 1);
        h.send(Command::SendFile {
            path: big.0.clone(),
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(
                "o arquivo passa do limite de 50 MB".to_owned()
            ))
        );
        let empty = TempFile::new("vazio.bin", 0);
        h.send(Command::SendFile {
            path: empty.0.clone(),
        });
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        h.send(Command::SendFile {
            path: big.0.with_file_name("nao-existe.bin"),
        });
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        h.send(Command::SendFile {
            path: big.0.parent().unwrap().to_path_buf(),
        });
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        assert!(send_file_calls(&h).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn file_at_the_limit_is_accepted() {
        let mut h = harness();
        h.join("geral").await;
        let file = TempFile::new("limite.bin", MAX_FILE_BYTES);
        h.send(Command::SendFile {
            path: file.0.clone(),
        });
        assert!(matches!(h.next().await, Event::FileTransfer(_)));
        assert_eq!(send_file_calls(&h).len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn send_file_outside_a_room_fails() {
        let mut h = harness();
        let file = TempFile::new("a.bin", 10);
        h.send(Command::SendFile {
            path: file.0.clone(),
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::NotInRoom)
        );
        assert!(send_file_calls(&h).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn backend_file_events_are_forwarded_unchanged() {
        let mut h = harness();
        h.join("geral").await;
        let transfer = FileTransfer {
            id: "t1".to_owned(),
            sender: "ana".to_owned(),
            name: "x.bin".to_owned(),
            size: 5,
            mime: "application/octet-stream".to_owned(),
            outgoing: false,
            sent_at_unix_ms: 3,
            state: TransferState::Done {
                path: PathBuf::from("x"),
            },
        };
        h.backend.emit(BackendEvent::FileTransfer(transfer.clone()));
        assert_eq!(h.next().await, Event::FileTransfer(transfer));
    }
}
