use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use zenha_protocol::{DEAFENED_ATTR, VoiceState, parse_deafened_attribute};

use crate::config::Config;
use crate::token::{base64_standard_encode, mint_server_token};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
pub const DEFAULT_OCCUPANTS_RECONCILE_TTL: Duration = Duration::from_secs(60);

// Values from `livekit_protocol::TrackSource`.
pub const TRACK_SOURCE_CAMERA: i32 = 1;
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

struct OccupantsCacheInner {
    rooms: HashMap<String, Vec<Occupant>>,
    last_full_sync: Option<Instant>,
}

pub struct OccupantsCache {
    reconcile_ttl: Duration,
    inner: Mutex<OccupantsCacheInner>,
}

impl Default for OccupantsCache {
    fn default() -> Self {
        Self::new(DEFAULT_OCCUPANTS_RECONCILE_TTL)
    }
}

impl OccupantsCache {
    pub fn new(reconcile_ttl: Duration) -> Self {
        Self {
            reconcile_ttl,
            inner: Mutex::new(OccupantsCacheInner {
                rooms: HashMap::new(),
                last_full_sync: None,
            }),
        }
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, OccupantsCacheInner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub fn get_if_fresh(&self, room_ids: &[String], now: Instant) -> Option<Vec<Vec<Occupant>>> {
        let guard = self.lock_inner();
        let synced_at = guard.last_full_sync?;
        if now.saturating_duration_since(synced_at) >= self.reconcile_ttl {
            return None;
        }
        Some(
            room_ids
                .iter()
                .map(|id| guard.rooms.get(id).cloned().unwrap_or_default())
                .collect(),
        )
    }

    pub fn replace_all(&self, entries: HashMap<String, Vec<Occupant>>, now: Instant) {
        let mut guard = self.lock_inner();
        guard.rooms = entries;
        guard.last_full_sync = Some(now);
    }

    pub fn update_room(&self, room: String, occupants: Vec<Occupant>) {
        let mut guard = self.lock_inner();
        guard.rooms.insert(room, occupants);
    }

    pub fn clear_room(&self, room: &str) {
        let mut guard = self.lock_inner();
        guard.rooms.remove(room);
    }
}

pub struct LiveKitDirectory {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    api_secret: String,
}

#[derive(Debug, thiserror::Error)]
enum TwirpError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Other(String),
}

#[derive(Deserialize)]
struct TwirpErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    msg: String,
}

#[derive(Serialize)]
struct RoomRequest<'a> {
    room: &'a str,
}

#[derive(Serialize)]
struct RoomParticipantRequest<'a> {
    room: &'a str,
    identity: &'a str,
}

#[derive(Serialize)]
struct ListRoomsRequest {
    names: Vec<String>,
}

#[derive(Serialize)]
struct MuteTrackRequest<'a> {
    room: &'a str,
    identity: &'a str,
    track_sid: &'a str,
    muted: bool,
}

#[derive(Serialize)]
struct SendDataRequest<'a> {
    room: &'a str,
    data: String,
    kind: &'static str,
    topic: &'a str,
    destination_identities: Vec<String>,
}

#[derive(Default, Deserialize)]
struct ListParticipantsResponse {
    #[serde(default)]
    participants: Vec<TwirpParticipant>,
}

#[derive(Default, Deserialize)]
struct ListRoomsResponse {
    #[serde(default)]
    rooms: Vec<TwirpRoom>,
}

#[derive(Default, Deserialize)]
struct TwirpRoom {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct TwirpParticipant {
    #[serde(default)]
    identity: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    tracks: Vec<TwirpTrack>,
    #[serde(default)]
    attributes: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct TwirpTrack {
    #[serde(default)]
    sid: String,
    #[serde(default)]
    muted: bool,
    #[serde(default, deserialize_with = "deserialize_track_source")]
    source: i32,
}

fn deserialize_track_source<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    struct TrackSourceVisitor;

    impl Visitor<'_> for TrackSourceVisitor {
        type Value = i32;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a LiveKit TrackSource enum string or integer")
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<i32, E> {
            i32::try_from(value).map_err(|_| E::custom("track source out of range"))
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<i32, E> {
            i32::try_from(value).map_err(|_| E::custom("track source out of range"))
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<i32, E> {
            Ok(match value {
                "CAMERA" => TRACK_SOURCE_CAMERA,
                "MICROPHONE" => TRACK_SOURCE_MICROPHONE,
                "SCREEN_SHARE" => TRACK_SOURCE_SCREEN_SHARE,
                "SCREEN_SHARE_AUDIO" => TRACK_SOURCE_SCREEN_SHARE_AUDIO,
                _ => value.parse::<i32>().unwrap_or(0),
            })
        }
    }

    deserializer.deserialize_any(TrackSourceVisitor)
}

fn livekit_http_base(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed
        .as_bytes()
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"wss://"))
    {
        format!("https://{}", &trimmed[6..])
    } else if trimmed
        .as_bytes()
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"ws://"))
    {
        format!("http://{}", &trimmed[5..])
    } else {
        trimmed.to_owned()
    }
}

fn parse_twirp_error(status: reqwest::StatusCode, body: &[u8]) -> TwirpError {
    if let Ok(parsed) = serde_json::from_slice::<TwirpErrorBody>(body) {
        if status == reqwest::StatusCode::NOT_FOUND || parsed.code == "not_found" {
            return TwirpError::NotFound;
        }
        if !parsed.code.is_empty() || !parsed.msg.is_empty() {
            return TwirpError::Other(format!(
                "twirp {} ({}): {}",
                parsed.code, status, parsed.msg
            ));
        }
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return TwirpError::NotFound;
    }
    TwirpError::Other(format!("twirp http status {status}"))
}

fn participant_to_occupant(p: TwirpParticipant) -> Occupant {
    Occupant {
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
    }
}

impl LiveKitDirectory {
    pub fn new(config: &Config) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            client,
            base_url: livekit_http_base(&config.livekit_url),
            api_key: config.livekit_api_key.clone(),
            api_secret: config.livekit_api_secret.clone(),
        })
    }

    async fn twirp_call<Req: Serialize, Res: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        room: Option<&str>,
        request: &Req,
    ) -> Result<Res, TwirpError> {
        let bytes = self.twirp_post(method, room, request).await?;
        serde_json::from_slice(&bytes)
            .map_err(|err| TwirpError::Other(format!("invalid twirp json for {method}: {err}")))
    }

    async fn twirp_call_empty<Req: Serialize>(
        &self,
        method: &str,
        room: Option<&str>,
        request: &Req,
    ) -> Result<(), TwirpError> {
        let _ = self.twirp_post(method, room, request).await?;
        Ok(())
    }

    async fn twirp_post<Req: Serialize>(
        &self,
        method: &str,
        room: Option<&str>,
        request: &Req,
    ) -> Result<Vec<u8>, TwirpError> {
        let jwt = mint_server_token(&self.api_key, &self.api_secret, room)
            .map_err(|err| TwirpError::Other(format!("minting server jwt failed: {err}")))?;
        let url = format!("{}/twirp/livekit.RoomService/{method}", self.base_url);
        let response = self
            .client
            .post(url)
            .bearer_auth(jwt)
            .json(request)
            .send()
            .await
            .map_err(|err| TwirpError::Other(err.to_string()))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|err| TwirpError::Other(err.to_string()))?;
        if !status.is_success() {
            return Err(parse_twirp_error(status, &body));
        }
        Ok(body.to_vec())
    }

    async fn get_participant(
        &self,
        room: &str,
        identity: &str,
    ) -> Result<TwirpParticipant, TwirpError> {
        self.twirp_call(
            "GetParticipant",
            Some(room),
            &RoomParticipantRequest { room, identity },
        )
        .await
    }

    async fn mute_sources(
        &self,
        room: &str,
        identity: &str,
        sources: &[i32],
        ignore_missing_track: bool,
    ) -> Result<(), ModerationError> {
        let participant = self
            .get_participant(room, identity)
            .await
            .map_err(map_moderation_error)?;
        for track in participant
            .tracks
            .into_iter()
            .filter(|t| sources.contains(&t.source))
        {
            match self
                .twirp_call_empty(
                    "MutePublishedTrack",
                    Some(room),
                    &MuteTrackRequest {
                        room,
                        identity,
                        track_sid: &track.sid,
                        muted: true,
                    },
                )
                .await
            {
                Ok(()) => {}
                Err(TwirpError::NotFound) if ignore_missing_track => {}
                Err(err) => return Err(map_moderation_error(err)),
            }
        }
        Ok(())
    }
}

fn map_moderation_error(err: TwirpError) -> ModerationError {
    match err {
        TwirpError::NotFound => ModerationError::NotFound,
        TwirpError::Other(msg) => ModerationError::Backend(msg),
    }
}

#[async_trait]
impl RoomDirectory for LiveKitDirectory {
    async fn participants(&self, room: &str) -> Result<Vec<Occupant>, DirectoryError> {
        match self
            .twirp_call::<_, ListParticipantsResponse>(
                "ListParticipants",
                Some(room),
                &RoomRequest { room },
            )
            .await
        {
            Ok(resp) => Ok(resp
                .participants
                .into_iter()
                .map(participant_to_occupant)
                .collect()),
            Err(TwirpError::NotFound) => Ok(Vec::new()),
            Err(TwirpError::Other(msg)) => Err(DirectoryError::Backend(msg)),
        }
    }
}

#[async_trait]
impl RoomModerator for LiveKitDirectory {
    async fn list_active_rooms(&self) -> Result<Vec<String>, ModerationError> {
        let resp: ListRoomsResponse = self
            .twirp_call("ListRooms", None, &ListRoomsRequest { names: Vec::new() })
            .await
            .map_err(|err| ModerationError::Backend(err.to_string()))?;
        Ok(resp.rooms.into_iter().map(|r| r.name).collect())
    }

    async fn delete_room(&self, room: &str) -> Result<(), ModerationError> {
        match self
            .twirp_call_empty("DeleteRoom", None, &RoomRequest { room })
            .await
        {
            Ok(()) | Err(TwirpError::NotFound) => Ok(()),
            Err(TwirpError::Other(msg)) => Err(ModerationError::Backend(msg)),
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
        self.get_participant(room, identity)
            .await
            .map(|_| ())
            .map_err(map_moderation_error)
    }

    async fn remove_participant(&self, room: &str, identity: &str) -> Result<(), ModerationError> {
        self.twirp_call_empty(
            "RemoveParticipant",
            Some(room),
            &RoomParticipantRequest { room, identity },
        )
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
        self.twirp_call_empty(
            "SendData",
            Some(room),
            &SendDataRequest {
                room,
                data: base64_standard_encode(&payload),
                kind: "RELIABLE",
                topic,
                destination_identities,
            },
        )
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
        participants_calls: Mutex<Vec<String>>,
    }

    impl FakeDirectory {
        pub fn with_participants(self, room: &str, names: &[&str]) -> Self {
            self.set_participants(room, names);
            self
        }

        pub fn set_participants(&self, room: &str, names: &[&str]) {
            self.rooms.lock().unwrap().insert(
                room.to_owned(),
                names.iter().map(|n| (*n).to_owned()).collect(),
            );
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

        pub fn participants_calls(&self) -> Vec<String> {
            self.participants_calls.lock().unwrap().clone()
        }

        pub fn clear_participants_calls(&self) {
            self.participants_calls.lock().unwrap().clear();
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
            self.participants_calls
                .lock()
                .unwrap()
                .push(room.to_owned());
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use zenha_protocol::VoiceState;

    use super::*;

    #[test]
    fn livekit_http_base_converts_ws_and_wss_and_trims_slash() {
        assert_eq!(
            livekit_http_base("ws://livekit.test:7880/"),
            "http://livekit.test:7880"
        );
        assert_eq!(
            livekit_http_base("wss://livekit.example.com"),
            "https://livekit.example.com"
        );
        assert_eq!(
            livekit_http_base("https://livekit.example.com/"),
            "https://livekit.example.com"
        );
    }

    #[test]
    fn twirp_list_participants_parses_string_and_integer_track_sources() {
        let json = r#"{
            "participants": [
                {
                    "identity": "id-ana",
                    "name": "Ana",
                    "tracks": [
                        {"sid": "TR_1", "muted": true, "source": "MICROPHONE"},
                        {"sid": "TR_2", "muted": false, "source": "SCREEN_SHARE"}
                    ],
                    "attributes": {"zenha.deafened": "1"}
                },
                {
                    "identity": "id-bia",
                    "name": "",
                    "tracks": [
                        {"sid": "TR_3", "muted": false, "source": 2},
                        {"sid": "TR_4", "muted": true, "source": 4}
                    ]
                },
                {
                    "identity": "id-caio",
                    "name": "Caio",
                    "tracks": [
                        {"sid": "TR_5", "muted": true, "source": 2}
                    ]
                }
            ]
        }"#;
        let parsed: ListParticipantsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.participants[0].tracks[0].source,
            TRACK_SOURCE_MICROPHONE
        );
        assert_eq!(
            parsed.participants[0].tracks[1].source,
            TRACK_SOURCE_SCREEN_SHARE
        );
        assert_eq!(
            parsed.participants[1].tracks[0].source,
            TRACK_SOURCE_MICROPHONE
        );
        assert_eq!(
            parsed.participants[1].tracks[1].source,
            TRACK_SOURCE_SCREEN_SHARE_AUDIO
        );

        let occupants: Vec<Occupant> = parsed
            .participants
            .into_iter()
            .map(participant_to_occupant)
            .collect();
        assert_eq!(
            occupants,
            vec![
                Occupant {
                    name: "Ana".to_owned(),
                    voice: VoiceState {
                        muted: true,
                        deafened: true,
                    },
                },
                Occupant {
                    name: "id-bia".to_owned(),
                    voice: VoiceState::default(),
                },
                Occupant {
                    name: "Caio".to_owned(),
                    voice: VoiceState {
                        muted: true,
                        deafened: false,
                    },
                },
            ]
        );
    }

    #[test]
    fn twirp_error_detects_not_found_by_code_or_status() {
        let by_code = parse_twirp_error(
            reqwest::StatusCode::BAD_REQUEST,
            br#"{"code":"not_found","msg":"requested room does not exist"}"#,
        );
        assert!(matches!(by_code, TwirpError::NotFound));
        assert!(matches!(
            map_moderation_error(by_code),
            ModerationError::NotFound
        ));

        let by_status = parse_twirp_error(reqwest::StatusCode::NOT_FOUND, b"");
        assert!(matches!(by_status, TwirpError::NotFound));

        let other = parse_twirp_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            br#"{"code":"internal","msg":"boom"}"#,
        );
        assert!(matches!(other, TwirpError::Other(msg) if msg.contains("boom")));
    }

    #[test]
    fn occupants_cache_serves_fresh_updates_clears_and_expires() {
        let cache = OccupantsCache::new(Duration::from_secs(60));
        let rooms = vec!["geral".to_owned(), "jogos".to_owned()];
        let t0 = Instant::now();
        assert!(cache.get_if_fresh(&rooms, t0).is_none());

        let ana = Occupant {
            name: "ana".to_owned(),
            voice: VoiceState::default(),
        };
        let bia = Occupant {
            name: "bia".to_owned(),
            voice: VoiceState::default(),
        };
        cache.replace_all(HashMap::from([("geral".to_owned(), vec![ana.clone()])]), t0);
        assert_eq!(
            cache.get_if_fresh(&rooms, t0 + Duration::from_secs(10)),
            Some(vec![vec![ana.clone()], vec![]])
        );

        cache.update_room("jogos".to_owned(), vec![bia.clone()]);
        assert_eq!(
            cache.get_if_fresh(&rooms, t0 + Duration::from_secs(20)),
            Some(vec![vec![ana], vec![bia]])
        );

        cache.clear_room("geral");
        assert_eq!(
            cache.get_if_fresh(&rooms, t0 + Duration::from_secs(30)),
            Some(vec![
                vec![],
                vec![Occupant {
                    name: "bia".to_owned(),
                    voice: VoiceState::default()
                }]
            ])
        );

        assert!(
            cache
                .get_if_fresh(&rooms, t0 + Duration::from_secs(60))
                .is_none()
        );
    }
}
