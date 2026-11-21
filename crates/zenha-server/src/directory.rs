use async_trait::async_trait;
use livekit_api::services::ServerError;
use livekit_api::services::ServerErrorCode;
use livekit_api::services::ServiceError;
use livekit_api::services::room::{RoomClient, SendDataOptions};
use zenha_protocol::{DEAFENED_ATTR, VoiceState, parse_deafened_attribute};

use crate::config::Config;

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

// Values from `livekit_protocol::TrackSource` (`livekit-protocol 0.8.2`).
pub const TRACK_SOURCE_MICROPHONE: i32 = 2;
pub const TRACK_SOURCE_SCREEN_SHARE: i32 = 3;
pub const TRACK_SOURCE_SCREEN_SHARE_AUDIO: i32 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupant {
    pub name: String,
    pub voice: VoiceState,
}

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("room directory lookup failed: {0}")]
    Backend(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ModerationError {
    #[error("room or participant not found")]
    NotFound,
    #[error("livekit moderation failed: {0}")]
    Backend(String),
}

#[async_trait]
pub trait RoomDirectory: Send + Sync {
    async fn participants(&self, room: &str) -> Result<Vec<Occupant>, DirectoryError>;
}

#[async_trait]
pub trait RoomModerator: Send + Sync {
    async fn list_active_rooms(&self) -> Result<Vec<String>, ModerationError>;
    async fn delete_room(&self, room: &str) -> Result<(), ModerationError>;
    async fn mute_microphone(&self, room: &str, identity: &str) -> Result<(), ModerationError>;
    async fn mute_screen_share(&self, room: &str, identity: &str) -> Result<(), ModerationError>;
    async fn ensure_participant(&self, room: &str, identity: &str) -> Result<(), ModerationError>;
    async fn remove_participant(&self, room: &str, identity: &str) -> Result<(), ModerationError>;
    async fn send_data(
        &self,
        room: &str,
        topic: &str,
        payload: Vec<u8>,
        destination_identities: Vec<String>,
    ) -> Result<(), ModerationError>;
}

pub struct LiveKitDirectory {
    client: RoomClient,
}

impl LiveKitDirectory {
    pub fn new(config: &Config) -> Self {
        let client = RoomClient::with_api_key(
            &config.livekit_url,
            &config.livekit_api_key,
            &config.livekit_api_secret,
        )
        .with_request_timeout(REQUEST_TIMEOUT);
        Self { client }
    }

    async fn mute_sources(
        &self,
        room: &str,
        identity: &str,
        sources: &[i32],
        ignore_missing_track: bool,
    ) -> Result<(), ModerationError> {
        let participant = self
            .client
            .get_participant(room, identity)
            .await
            .map_err(map_moderation_error)?;
        for track in participant
            .tracks
            .into_iter()
            .filter(|t| sources.contains(&t.source))
        {
            match self
                .client
                .mute_published_track(room, identity, &track.sid, true)
                .await
            {
                Ok(_) => {}
                Err(err) if ignore_missing_track && is_not_found(&err) => {}
                Err(err) => return Err(map_moderation_error(err)),
            }
        }
        Ok(())
    }
}

fn is_not_found(err: &ServiceError) -> bool {
    matches!(
        err,
        ServiceError::Twirp(ServerError::Twirp(code)) if code.code == ServerErrorCode::NOT_FOUND
    )
}

fn map_moderation_error(err: ServiceError) -> ModerationError {
    if is_not_found(&err) {
        ModerationError::NotFound
    } else {
        ModerationError::Backend(err.to_string())
    }
}

#[async_trait]
impl RoomDirectory for LiveKitDirectory {
    async fn participants(&self, room: &str) -> Result<Vec<Occupant>, DirectoryError> {
        match self.client.list_participants(room).await {
            Ok(participants) => Ok(participants
                .into_iter()
                .map(|p| Occupant {
                    voice: VoiceState {
                        muted: p
                            .tracks
                            .iter()
                            .any(|track| track.source == TRACK_SOURCE_MICROPHONE && track.muted),
                        deafened: p
                            .attributes
                            .get(DEAFENED_ATTR)
                            .is_some_and(|raw| parse_deafened_attribute(raw)),
                    },
                    name: if p.name.is_empty() {
                        p.identity
                    } else {
                        p.name
                    },
                })
                .collect()),
            Err(err) if is_not_found(&err) => Ok(Vec::new()),
            Err(err) => Err(DirectoryError::Backend(err.to_string())),
        }
    }
}

#[async_trait]
impl RoomModerator for LiveKitDirectory {
    async fn list_active_rooms(&self) -> Result<Vec<String>, ModerationError> {
        let rooms = self
            .client
            .list_rooms(Vec::new())
            .await
            .map_err(|err| ModerationError::Backend(err.to_string()))?;
        Ok(rooms.into_iter().map(|r| r.name).collect())
    }

    async fn delete_room(&self, room: &str) -> Result<(), ModerationError> {
        match self.client.delete_room(room).await {
            Ok(()) => Ok(()),
            Err(err) if is_not_found(&err) => Ok(()),
            Err(err) => Err(ModerationError::Backend(err.to_string())),
        }
    }

    async fn mute_microphone(&self, room: &str, identity: &str) -> Result<(), ModerationError> {
        self.mute_sources(room, identity, &[TRACK_SOURCE_MICROPHONE], false)
            .await
    }

    async fn mute_screen_share(&self, room: &str, identity: &str) -> Result<(), ModerationError> {
        self.mute_sources(
            room,
            identity,
            &[TRACK_SOURCE_SCREEN_SHARE, TRACK_SOURCE_SCREEN_SHARE_AUDIO],
            true,
        )
        .await
    }

    async fn ensure_participant(&self, room: &str, identity: &str) -> Result<(), ModerationError> {
        self.client
            .get_participant(room, identity)
            .await
            .map(|_| ())
            .map_err(map_moderation_error)
    }

    async fn remove_participant(&self, room: &str, identity: &str) -> Result<(), ModerationError> {
        self.client
            .remove_participant(room, identity)
            .await
            .map_err(map_moderation_error)
    }

    async fn send_data(
        &self,
        room: &str,
        topic: &str,
        payload: Vec<u8>,
        destination_identities: Vec<String>,
    ) -> Result<(), ModerationError> {
        // `SendDataOptions::default()` uses `proto::data_packet::Kind::Reliable`.
        let options = SendDataOptions {
            topic: Some(topic.to_owned()),
            destination_identities,
            ..Default::default()
        };
        self.client
            .send_data(room, payload, options)
            .await
            .map_err(map_moderation_error)
    }
}

#[cfg(test)]
pub use fake::{FakeDirectory, ModerationCall};

#[cfg(test)]
mod fake {
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::{
        DirectoryError, ModerationError, Occupant, RoomDirectory, RoomModerator,
        TRACK_SOURCE_MICROPHONE, TRACK_SOURCE_SCREEN_SHARE, TRACK_SOURCE_SCREEN_SHARE_AUDIO,
        VoiceState,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ModerationCall {
        DeleteRoom(String),
        MuteTrack {
            room: String,
            identity: String,
            track_sid: String,
        },
        RemoveParticipant {
            room: String,
            identity: String,
        },
        SendData {
            room: String,
            topic: String,
            payload: Vec<u8>,
            destination_identities: Vec<String>,
        },
    }

    #[derive(Debug, Clone)]
    struct FakeTrack {
        sid: String,
        source: i32,
    }

    #[derive(Default)]
    pub struct FakeDirectory {
        rooms: Mutex<HashMap<String, Vec<String>>>,
        tracks: Mutex<HashMap<(String, String), Vec<FakeTrack>>>,
        voices: Mutex<HashMap<(String, String), VoiceState>>,
        failing: HashSet<String>,
        fail_moderation: bool,
        fail_send_data: bool,
        calls: Mutex<Vec<ModerationCall>>,
    }

    impl FakeDirectory {
        pub fn with_participants(self, room: &str, names: &[&str]) -> Self {
            self.rooms.lock().unwrap().insert(
                room.to_owned(),
                names.iter().map(|n| (*n).to_owned()).collect(),
            );
            self
        }

        pub fn with_voice(self, room: &str, name: &str, voice: VoiceState) -> Self {
            self.voices
                .lock()
                .unwrap()
                .insert((room.to_owned(), name.to_owned()), voice);
            self
        }

        pub fn with_track(self, room: &str, identity: &str, sid: &str, source: i32) -> Self {
            self.tracks
                .lock()
                .unwrap()
                .entry((room.to_owned(), identity.to_owned()))
                .or_default()
                .push(FakeTrack {
                    sid: sid.to_owned(),
                    source,
                });
            self
        }

        pub fn failing_on(mut self, room: &str) -> Self {
            self.failing.insert(room.to_owned());
            self
        }

        pub fn failing_moderation(mut self) -> Self {
            self.fail_moderation = true;
            self
        }

        pub fn failing_send_data(mut self) -> Self {
            self.fail_send_data = true;
            self
        }

        pub fn calls(&self) -> Vec<ModerationCall> {
            self.calls.lock().unwrap().clone()
        }

        fn has_participant(&self, room: &str, identity: &str) -> bool {
            self.rooms
                .lock()
                .unwrap()
                .get(room)
                .is_some_and(|ps| ps.iter().any(|p| p == identity))
        }

        fn mute_sources(
            &self,
            room: &str,
            identity: &str,
            sources: &[i32],
        ) -> Result<(), ModerationError> {
            if self.fail_moderation || self.failing.contains(room) {
                return Err(ModerationError::Backend("fake failure".to_owned()));
            }
            if !self.has_participant(room, identity) {
                return Err(ModerationError::NotFound);
            }
            let matching: Vec<String> = self
                .tracks
                .lock()
                .unwrap()
                .get(&(room.to_owned(), identity.to_owned()))
                .into_iter()
                .flatten()
                .filter(|t| sources.contains(&t.source))
                .map(|t| t.sid.clone())
                .collect();
            let mut calls = self.calls.lock().unwrap();
            for track_sid in matching {
                calls.push(ModerationCall::MuteTrack {
                    room: room.to_owned(),
                    identity: identity.to_owned(),
                    track_sid,
                });
            }
            Ok(())
        }
    }

    #[async_trait]
    impl RoomDirectory for FakeDirectory {
        async fn participants(&self, room: &str) -> Result<Vec<Occupant>, DirectoryError> {
            if self.failing.contains(room) {
                return Err(DirectoryError::Backend("fake failure".to_owned()));
            }
            let names = self
                .rooms
                .lock()
                .unwrap()
                .get(room)
                .cloned()
                .unwrap_or_default();
            let voices = self.voices.lock().unwrap();
            Ok(names
                .into_iter()
                .map(|name| Occupant {
                    voice: voices
                        .get(&(room.to_owned(), name.clone()))
                        .copied()
                        .unwrap_or_default(),
                    name,
                })
                .collect())
        }
    }

    #[async_trait]
    impl RoomModerator for FakeDirectory {
        async fn list_active_rooms(&self) -> Result<Vec<String>, ModerationError> {
            if self.fail_moderation {
                return Err(ModerationError::Backend("fake failure".to_owned()));
            }
            let mut active: Vec<String> = self
                .rooms
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, participants)| !participants.is_empty())
                .map(|(room, _)| room.clone())
                .collect();
            active.sort();
            Ok(active)
        }

        async fn delete_room(&self, room: &str) -> Result<(), ModerationError> {
            if self.fail_moderation || self.failing.contains(room) {
                return Err(ModerationError::Backend("fake failure".to_owned()));
            }
            self.rooms.lock().unwrap().remove(room);
            self.calls
                .lock()
                .unwrap()
                .push(ModerationCall::DeleteRoom(room.to_owned()));
            Ok(())
        }

        async fn mute_microphone(&self, room: &str, identity: &str) -> Result<(), ModerationError> {
            self.mute_sources(room, identity, &[TRACK_SOURCE_MICROPHONE])
        }

        async fn mute_screen_share(
            &self,
            room: &str,
            identity: &str,
        ) -> Result<(), ModerationError> {
            self.mute_sources(
                room,
                identity,
                &[TRACK_SOURCE_SCREEN_SHARE, TRACK_SOURCE_SCREEN_SHARE_AUDIO],
            )
        }

        async fn ensure_participant(
            &self,
            room: &str,
            identity: &str,
        ) -> Result<(), ModerationError> {
            if self.fail_moderation || self.failing.contains(room) {
                return Err(ModerationError::Backend("fake failure".to_owned()));
            }
            if self.has_participant(room, identity) {
                Ok(())
            } else {
                Err(ModerationError::NotFound)
            }
        }

        async fn remove_participant(
            &self,
            room: &str,
            identity: &str,
        ) -> Result<(), ModerationError> {
            if self.fail_moderation || self.failing.contains(room) {
                return Err(ModerationError::Backend("fake failure".to_owned()));
            }
            let mut rooms = self.rooms.lock().unwrap();
            let Some(ps) = rooms.get_mut(room) else {
                return Err(ModerationError::NotFound);
            };
            let Some(idx) = ps.iter().position(|p| p == identity) else {
                return Err(ModerationError::NotFound);
            };
            ps.remove(idx);
            self.calls
                .lock()
                .unwrap()
                .push(ModerationCall::RemoveParticipant {
                    room: room.to_owned(),
                    identity: identity.to_owned(),
                });
            Ok(())
        }

        async fn send_data(
            &self,
            room: &str,
            topic: &str,
            payload: Vec<u8>,
            destination_identities: Vec<String>,
        ) -> Result<(), ModerationError> {
            if self.fail_moderation || self.fail_send_data || self.failing.contains(room) {
                return Err(ModerationError::Backend("fake failure".to_owned()));
            }
            let rooms = self.rooms.lock().unwrap();
            let Some(ps) = rooms.get(room) else {
                return Err(ModerationError::NotFound);
            };
            if ps.is_empty()
                || (!destination_identities.is_empty()
                    && !destination_identities.iter().all(|id| ps.contains(id)))
            {
                return Err(ModerationError::NotFound);
            }
            self.calls.lock().unwrap().push(ModerationCall::SendData {
                room: room.to_owned(),
                topic: topic.to_owned(),
                payload,
                destination_identities,
            });
            Ok(())
        }
    }
}
