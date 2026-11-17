//! Envio e recebimento de arquivos pelos byte streams do LiveKit.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use futures_util::StreamExt;
use livekit::{ByteStreamReader, Room, SendByteOptions, StreamReader};
use zenha_protocol::MAX_FILE_BYTES;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{FileTransfer, TransferState};
use crate::backend::BackendEvent;
use crate::clock::unix_ms;
use crate::file_receive::{FileReceiver, IncomingFile, PROGRESS_INTERVAL, ReceiveSlot, Throttle};
use crate::files::{FILE_TOPIC, mime_for_name, received_path, sanitize_file_name};

const MAX_ID_CHARS: usize = 64;

fn emit(events: &UnboundedSender<BackendEvent>, transfer: &FileTransfer, state: TransferState) {
    let _ = events.send(BackendEvent::FileTransfer(FileTransfer {
        state,
        ..transfer.clone()
    }));
}

pub fn start_send(
    room: Arc<Room>,
    events: UnboundedSender<BackendEvent>,
    transfer: FileTransfer,
    path: PathBuf,
) {
    tokio::spawn(async move {
        let throttle = Mutex::new(Throttle::new(PROGRESS_INTERVAL));
        let progress_events = events.clone();
        let progress_base = transfer.clone();
        let options = SendByteOptions::new_with_topic(FILE_TOPIC)
            .with_id(transfer.id.clone())
            .with_name(transfer.name.clone())
            .with_mime_type(transfer.mime.clone())
            .with_compress(false)
            .with_on_progress(move |progress| {
                let ready = throttle
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .ready(Instant::now());
                if ready {
                    emit(
                        &progress_events,
                        &progress_base,
                        TransferState::InProgress {
                            bytes: progress.bytes_processed(),
                        },
                    );
                }
            });
        match room.local_participant().send_file(&path, options).await {
            Ok(_) => emit(&events, &transfer, TransferState::Done { path }),
            Err(err) => {
                tracing::warn!(%err, id = %transfer.id, "sending file failed");
                emit(&events, &transfer, TransferState::Failed(err.to_string()));
            }
        }
    });
}

pub fn start_receive(
    reader: ByteStreamReader,
    sender: String,
    files: &FileReceiver,
    events: &UnboundedSender<BackendEvent>,
) {
    let info = reader.info();
    let Some(size) = info
        .total_length
        .filter(|length| (1..=MAX_FILE_BYTES).contains(length))
    else {
        tracing::warn!(
            sender,
            length = ?info.total_length,
            "dropping incoming file with missing or invalid length"
        );
        return;
    };
    let name = sanitize_file_name(&info.name);
    let mime = mime_for_name(&name).to_owned();
    let id: String = info.id.chars().take(MAX_ID_CHARS).collect();
    let path = received_path(files.dir(), &id, &name);
    let Some(slot) = files.try_acquire() else {
        tracing::warn!(sender, "too many incoming files at once, dropping one");
        return;
    };
    let transfer = FileTransfer {
        id,
        sender,
        name,
        size,
        mime,
        outgoing: false,
        sent_at_unix_ms: unix_ms(),
        state: TransferState::InProgress { bytes: 0 },
    };
    emit(events, &transfer, TransferState::InProgress { bytes: 0 });
    tokio::spawn(receive(reader, path, transfer, slot, events.clone()));
}

async fn receive(
    mut reader: ByteStreamReader,
    path: PathBuf,
    transfer: FileTransfer,
    _slot: ReceiveSlot,
    events: UnboundedSender<BackendEvent>,
) {
    let fail = |message: &str| {
        emit(
            &events,
            &transfer,
            TransferState::Failed(message.to_owned()),
        )
    };
    let mut file = match IncomingFile::create(&path, transfer.size) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(%err, id = %transfer.id, "could not create the received file");
            fail(err.user_message());
            return;
        }
    };
    let mut throttle = Throttle::new(PROGRESS_INTERVAL);
    while let Some(chunk) = reader.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                tracing::warn!(%err, id = %transfer.id, "incoming file stream failed");
                fail("a transferência foi interrompida");
                return;
            }
        };
        match file.write_chunk(chunk.as_ref()) {
            Ok(bytes) => {
                if throttle.ready(Instant::now()) {
                    emit(&events, &transfer, TransferState::InProgress { bytes });
                }
            }
            Err(err) => {
                tracing::warn!(%err, id = %transfer.id, "writing the received file failed");
                fail(err.user_message());
                return;
            }
        }
    }
    match file.finish() {
        Ok(path) => emit(&events, &transfer, TransferState::Done { path }),
        Err(err) => {
            tracing::warn!(%err, id = %transfer.id, "incoming file is incomplete");
            fail(err.user_message());
        }
    }
}
