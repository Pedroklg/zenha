//! Envio de pacotes de dados e de arquivos pela sala.

use std::path::PathBuf;

use livekit::prelude::DataPacket;
use zenha_protocol::AVATAR_ATTR;

use crate::api::{FileTransfer, SessionError};
use crate::livekit::files;

use super::LiveKitBackend;

impl LiveKitBackend {
    pub(super) async fn publish_data(
        &self,
        topic: &str,
        payload: Vec<u8>,
    ) -> Result<(), SessionError> {
        let room = self.current_room().await.ok_or(SessionError::NotInRoom)?;
        let packet = DataPacket {
            payload,
            topic: Some(topic.to_owned()),
            reliable: true,
            destination_identities: Vec::new(),
        };
        room.local_participant()
            .publish_data(packet)
            .await
            .map_err(|err| {
                tracing::warn!(%err, "publishing data failed");
                SessionError::Connection("could not send data".to_owned())
            })
    }

    pub(super) async fn publish_name(&self, name: &str) -> Result<(), SessionError> {
        let room = self.current_room().await.ok_or(SessionError::NotInRoom)?;
        room.local_participant()
            .set_name(name.to_owned())
            .await
            .map_err(|err| {
                tracing::warn!(%err, "updating participant name failed");
                SessionError::Connection("could not update the display name".to_owned())
            })
    }

    pub(super) async fn publish_avatar_attribute(
        &self,
        version: Option<&str>,
    ) -> Result<(), SessionError> {
        let room = self.current_room().await.ok_or(SessionError::NotInRoom)?;
        let local = room.local_participant();
        let mut attributes = local.attributes();
        attributes.insert(
            AVATAR_ATTR.to_owned(),
            version.unwrap_or_default().to_owned(),
        );
        local.set_attributes(attributes).await.map_err(|err| {
            tracing::warn!(%err, "updating participant avatar attribute failed");
            SessionError::Connection("could not update the avatar attribute".to_owned())
        })
    }

    pub(super) async fn start_file_send(
        &self,
        transfer: FileTransfer,
        path: PathBuf,
    ) -> Result<(), SessionError> {
        let (room, events) = self
            .current_room_and_events()
            .await
            .ok_or(SessionError::NotInRoom)?;
        files::start_send(room, events, transfer, path);
        Ok(())
    }
}
