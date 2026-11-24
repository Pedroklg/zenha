//! Sending data packets and files through the room.

use std::path::PathBuf;

use super::p2p_signal::encode_p2p_signal;
use livekit::prelude::{DataPacket, ParticipantIdentity, Room};
use zenha_protocol::{AVATAR_ATTR, DEAFENED_ATTR, P2P_SIGNAL_TOPIC, P2pSignal, deafened_attribute};

use crate::api::{FileTransfer, SessionError};
use crate::livekit_backend::files;

use super::LiveKitBackend;

fn build_data_packet(topic: &str, payload: Vec<u8>, destinations: Vec<String>) -> DataPacket {
    DataPacket {
        payload,
        topic: Some(topic.to_owned()),
        reliable: true,
        destination_identities: destinations
            .into_iter()
            .map(ParticipantIdentity::from)
            .collect(),
    }
}

pub(super) async fn send_p2p_signal(
    room: &Room,
    destination: &str,
    signal: &P2pSignal,
) -> Result<(), SessionError> {
    let payload =
        encode_p2p_signal(signal).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
    let packet = build_data_packet(P2P_SIGNAL_TOPIC, payload, vec![destination.to_owned()]);
    room.local_participant()
        .publish_data(packet)
        .await
        .map_err(|err| {
            tracing::warn!(%err, destination, "publishing p2p signal failed");
            SessionError::Connection("could not send p2p signal".to_owned())
        })
}

impl LiveKitBackend {
    pub(super) async fn publish_data(
        &self,
        topic: &str,
        payload: Vec<u8>,
    ) -> Result<(), SessionError> {
        self.publish_data_to(topic, payload, Vec::new()).await
    }

    pub(super) async fn publish_data_to(
        &self,
        topic: &str,
        payload: Vec<u8>,
        destinations: Vec<String>,
    ) -> Result<(), SessionError> {
        let room = self.current_room().await.ok_or(SessionError::NotInRoom)?;
        let packet = build_data_packet(topic, payload, destinations);
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

    /// Tells the others the headphones are off; a failure only costs the indicator.
    pub(super) async fn publish_deafened_attribute(&self, room: &Room, deafened: bool) {
        let local = room.local_participant();
        let mut attributes = local.attributes();
        if super::room::deafened(&attributes) == deafened {
            return;
        }
        attributes.insert(
            DEAFENED_ATTR.to_owned(),
            deafened_attribute(deafened).to_owned(),
        );
        if let Err(err) = local.set_attributes(attributes).await {
            tracing::warn!(%err, "updating participant deafened attribute failed");
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_data_packet_sets_topic_reliability_and_destinations() {
        let broadcast = build_data_packet("chat", b"hi".to_vec(), Vec::new());
        assert_eq!(broadcast.topic.as_deref(), Some("chat"));
        assert!(broadcast.reliable);
        assert!(broadcast.destination_identities.is_empty());

        let targeted = build_data_packet(
            P2P_SIGNAL_TOPIC,
            b"{}".to_vec(),
            vec!["peer-b".to_owned(), "peer-c".to_owned()],
        );
        assert_eq!(targeted.topic.as_deref(), Some(P2P_SIGNAL_TOPIC));
        assert!(targeted.reliable);
        let identities: Vec<&str> = targeted
            .destination_identities
            .iter()
            .map(ParticipantIdentity::as_str)
            .collect();
        assert_eq!(identities, vec!["peer-b", "peer-c"]);
    }
}
