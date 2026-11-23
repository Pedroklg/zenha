//! Translation of LiveKit room events into `BackendEvent`.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::p2p_signal::decode_p2p_signal;
use livekit::prelude::{
    DataPacketKind, DisconnectReason, LocalTrack, Participant as SdkParticipant, RemoteTrack, Room,
    RoomEvent, TrackPublication, TrackSource,
};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use zenha_protocol::{
    AVATAR_ATTR, DEAFENED_ATTR, P2P_SIGNAL_TOPIC, P2pSignal, parse_avatar_attribute,
    parse_deafened_attribute,
};

use crate::api::RemovalReason;
use crate::backend::BackendEvent;
use crate::backend::support::{SpeakingTracker, display_name};
use crate::files::FILE_TOPIC;
use crate::files::receive::FileReceiver;
use crate::livekit_backend::files;
use crate::livekit_backend::video::Watch;

use super::Flags;
use super::audio::audio_tap;
use super::room::{
    is_screen_share, is_share_audio, remote_view, set_screen_share_subscribed, wants_audio,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IncomingP2pSignal {
    pub(super) sender: String,
    pub(super) signal: P2pSignal,
}

enum DataRoute {
    Ignore,
    Backend(BackendEvent),
    P2pSignal(IncomingP2pSignal),
}

fn route_data_packet(
    kind: DataPacketKind,
    topic: Option<String>,
    sender: Option<String>,
    payload: Vec<u8>,
) -> DataRoute {
    if topic.as_deref() == Some(P2P_SIGNAL_TOPIC) {
        if kind != DataPacketKind::Reliable {
            tracing::debug!("dropping lossy p2p signal packet");
            return DataRoute::Ignore;
        }
        let Some(sender) = sender else {
            tracing::warn!("ignoring anonymous p2p signal packet");
            return DataRoute::Ignore;
        };
        return match decode_p2p_signal(&payload) {
            Ok(signal) => DataRoute::P2pSignal(IncomingP2pSignal { sender, signal }),
            Err(err) => {
                tracing::warn!(%sender, %err, "dropping invalid p2p signal packet");
                DataRoute::Ignore
            }
        };
    }
    match (kind, topic) {
        (DataPacketKind::Reliable, Some(topic)) => DataRoute::Backend(BackendEvent::DataReceived {
            sender,
            topic,
            payload,
        }),
        _ => DataRoute::Ignore,
    }
}

fn mute_event(
    participant: &SdkParticipant,
    publication: &TrackPublication,
    muted: bool,
    flags: &Flags,
) -> Vec<BackendEvent> {
    if publication.source() != TrackSource::Microphone {
        return Vec::new();
    }
    if matches!(participant, SdkParticipant::Local(_)) {
        flags.microphone_muted.store(muted, Ordering::SeqCst);
    }
    vec![BackendEvent::MuteChanged {
        identity: String::from(participant.identity()),
        muted,
    }]
}

fn disconnect_event(reason: DisconnectReason) -> BackendEvent {
    match reason {
        DisconnectReason::ParticipantRemoved => BackendEvent::RemovedFromRoom {
            reason: RemovalReason::Kicked,
        },
        DisconnectReason::RoomDeleted => BackendEvent::RemovedFromRoom {
            reason: RemovalReason::RoomDeleted,
        },
        other => BackendEvent::Disconnected {
            reason: format!("{other:?}"),
        },
    }
}

fn translate(
    event: RoomEvent,
    room: &Arc<Room>,
    speaking: &mut SpeakingTracker,
    flags: &Flags,
    watch: &Watch,
    events: &UnboundedSender<BackendEvent>,
    files: &FileReceiver,
) -> Vec<BackendEvent> {
    match event {
        RoomEvent::ParticipantConnected(participant) => {
            vec![BackendEvent::ParticipantJoined(remote_view(&participant))]
        }
        RoomEvent::ParticipantDisconnected(participant) => {
            let identity = String::from(participant.identity());
            speaking.forget(&identity);
            watch.clear_if(&identity);
            vec![BackendEvent::ParticipantLeft { identity }]
        }
        RoomEvent::ParticipantNameChanged {
            participant, name, ..
        } => {
            let identity = String::from(participant.identity());
            vec![BackendEvent::NameChanged {
                name: display_name(name, &identity),
                identity,
            }]
        }
        RoomEvent::ParticipantAttributesChanged {
            participant,
            changed_attributes,
        } => {
            let identity = String::from(participant.identity());
            let mut events = Vec::new();
            if let Some(raw) = changed_attributes.get(AVATAR_ATTR) {
                events.push(BackendEvent::AvatarChanged {
                    identity: identity.clone(),
                    version: parse_avatar_attribute(raw),
                });
            }
            if let Some(raw) = changed_attributes.get(DEAFENED_ATTR) {
                events.push(BackendEvent::DeafenChanged {
                    identity,
                    deafened: parse_deafened_attribute(raw),
                });
            }
            events
        }
        RoomEvent::TrackMuted {
            participant,
            publication,
        } => mute_event(&participant, &publication, true, flags),
        RoomEvent::TrackUnmuted {
            participant,
            publication,
        } => mute_event(&participant, &publication, false, flags),
        RoomEvent::TrackPublished {
            participant,
            publication,
        } => {
            let identity = String::from(participant.identity());
            if wants_audio(&publication)
                || (is_share_audio(&publication) && watch.is_watching(&identity))
            {
                publication.set_subscribed(true);
            }
            if is_screen_share(&publication) {
                return vec![BackendEvent::RemoteScreenShareStarted { identity }];
            }
            if publication.source() != TrackSource::Microphone {
                return Vec::new();
            }
            vec![BackendEvent::MuteChanged {
                identity,
                muted: publication.is_muted(),
            }]
        }
        RoomEvent::TrackUnpublished {
            participant,
            publication,
        } => {
            if !is_screen_share(&publication) {
                return Vec::new();
            }
            let identity = String::from(participant.identity());
            watch.clear_if(&identity);
            vec![BackendEvent::RemoteScreenShareStopped { identity }]
        }
        RoomEvent::TrackSubscribed {
            track,
            publication,
            participant,
        } => {
            if let RemoteTrack::Audio(audio) = &track {
                let stream = is_share_audio(&publication);
                if std::env::var_os("ZENHA_AUDIO_TAP").is_some() {
                    tokio::spawn(audio_tap(
                        String::from(participant.identity()),
                        if stream { "stream" } else { "voice" },
                        NativeAudioStream::new(audio.rtc_track(), 48_000, 1),
                    ));
                }
                if stream {
                    flags.play_stream(audio.rtc_track());
                } else {
                    flags.attach_voice(
                        participant.identity().as_str(),
                        publication.sid().to_string(),
                        audio.rtc_track(),
                    );
                }
                let deafened = flags.output_muted.load(Ordering::SeqCst);
                if deafened {
                    publication.set_enabled(false);
                }
                tracing::info!(
                    identity = %String::from(participant.identity()),
                    deafened,
                    track_enabled = publication.is_enabled(),
                    "remote audio track subscribed"
                );
            } else if let RemoteTrack::Video(video) = &track
                && is_screen_share(&publication)
            {
                let identity = String::from(participant.identity());
                tracing::info!(%identity, "remote screen share track subscribed");
                watch.start_stream(
                    &identity,
                    video.rtc_track(),
                    events.clone(),
                    Arc::clone(&flags.ui_visible.0),
                );
            }
            Vec::new()
        }
        RoomEvent::TrackUnsubscribed {
            track,
            participant,
            publication,
        } => {
            if is_share_audio(&publication) {
                flags.stop_stream();
            } else {
                flags.detach_voice(&publication.sid().to_string());
            }
            if matches!(track, RemoteTrack::Audio(_)) {
                tracing::info!(
                    identity = %String::from(participant.identity()),
                    deafened = flags.output_muted.load(Ordering::SeqCst),
                    "remote audio track unsubscribed"
                );
            }
            Vec::new()
        }
        RoomEvent::TrackSubscriptionFailed {
            participant,
            error,
            track_sid,
        } => {
            let identity = String::from(participant.identity());
            tracing::warn!(%identity, ?error, "remote track subscription failed");
            if watch.failed_sid_matches(&identity, &track_sid) {
                flags.stop_stream();
                set_screen_share_subscribed(room, &identity, false);
                watch.clear();
                return vec![BackendEvent::WatchEnded {
                    identity,
                    reason: Some("could not subscribe to the screen share".to_owned()),
                }];
            }
            Vec::new()
        }
        RoomEvent::LocalTrackRepublished {
            previous_sid,
            publication,
            track,
            ..
        } => {
            let new_sid = publication.sid();
            match (track, publication.source()) {
                (LocalTrack::Audio(track), TrackSource::Microphone)
                    if flags.microphone_muted.load(Ordering::SeqCst) =>
                {
                    track.disable();
                }
                (LocalTrack::Video(_), TrackSource::Screenshare)
                    if flags
                        .share_sids
                        .republish_video(&previous_sid, new_sid.clone()) =>
                {
                    tracing::info!(
                        %previous_sid,
                        %new_sid,
                        "local screen share video track republished"
                    );
                }
                (LocalTrack::Audio(_), TrackSource::ScreenshareAudio)
                    if flags
                        .share_sids
                        .republish_audio(&previous_sid, new_sid.clone()) =>
                {
                    tracing::info!(
                        %previous_sid,
                        %new_sid,
                        "local screen share audio track republished"
                    );
                }
                _ => {}
            }
            Vec::new()
        }
        RoomEvent::ActiveSpeakersChanged { speakers } => {
            let now: HashSet<String> = speakers
                .iter()
                .map(|speaker| String::from(speaker.identity()))
                .collect();
            speaking.update(now)
        }
        RoomEvent::DataReceived {
            payload,
            topic,
            kind,
            participant,
        } => {
            let sender = participant.and_then(|sender| {
                let identity = sender.identity();
                if topic.as_deref() == Some(P2P_SIGNAL_TOPIC)
                    && !room.remote_participants().contains_key(&identity)
                {
                    tracing::warn!(
                        sender = %String::from(identity),
                        "ignoring p2p signal from participant outside room"
                    );
                    return None;
                }
                Some(String::from(identity))
            });
            match route_data_packet(kind, topic, sender, Arc::unwrap_or_clone(payload)) {
                DataRoute::Backend(event) => vec![event],
                DataRoute::P2pSignal(signal) => {
                    flags.forward_p2p_signal(signal);
                    Vec::new()
                }
                DataRoute::Ignore => Vec::new(),
            }
        }
        RoomEvent::ByteStreamOpened {
            reader,
            topic,
            participant_identity,
        } => {
            if topic == FILE_TOPIC
                && let Some(reader) = reader.take()
            {
                files::start_receive(reader, String::from(participant_identity), files, events);
            }
            Vec::new()
        }
        RoomEvent::Reconnecting => vec![BackendEvent::Reconnecting],
        RoomEvent::Reconnected => vec![BackendEvent::Reconnected],
        RoomEvent::Disconnected { reason } => vec![disconnect_event(reason.into())],
        _ => Vec::new(),
    }
}

pub(super) async fn pump_events(
    mut room_events: UnboundedReceiver<RoomEvent>,
    tx: UnboundedSender<BackendEvent>,
    room: Arc<Room>,
    flags: Arc<Flags>,
    watch: Arc<Watch>,
    files: Arc<FileReceiver>,
) {
    let mut speaking = SpeakingTracker::new(String::from(room.local_participant().identity()));
    while let Some(event) = room_events.recv().await {
        let mut finished = false;
        for backend_event in translate(event, &room, &mut speaking, &flags, &watch, &tx, &files) {
            finished |= matches!(
                backend_event,
                BackendEvent::Disconnected { .. } | BackendEvent::RemovedFromRoom { .. }
            );
            if tx.send(backend_event).is_err() {
                return;
            }
        }
        if finished {
            return;
        }
    }
    let _ = tx.send(BackendEvent::Disconnected {
        reason: "room event stream ended".to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_reason_maps_removed_and_room_deleted() {
        assert_eq!(
            disconnect_event(DisconnectReason::ParticipantRemoved),
            BackendEvent::RemovedFromRoom {
                reason: RemovalReason::Kicked
            }
        );
        assert_eq!(
            disconnect_event(DisconnectReason::RoomDeleted),
            BackendEvent::RemovedFromRoom {
                reason: RemovalReason::RoomDeleted
            }
        );
        assert_eq!(
            disconnect_event(DisconnectReason::ServerShutdown),
            BackendEvent::Disconnected {
                reason: "ServerShutdown".to_owned()
            }
        );
    }

    #[test]
    fn p2p_signal_topic_is_intercepted_and_never_forwarded_to_session() {
        let signal = P2pSignal::Request {
            share_epoch: 3,
            session_id: "sess-1".to_owned(),
        };
        let payload = super::super::p2p_signal::encode_p2p_signal(&signal).unwrap();

        match route_data_packet(
            DataPacketKind::Reliable,
            Some(P2P_SIGNAL_TOPIC.to_owned()),
            Some("peer-a".to_owned()),
            payload.clone(),
        ) {
            DataRoute::P2pSignal(incoming) => {
                assert_eq!(incoming.sender, "peer-a");
                assert_eq!(incoming.signal, signal);
            }
            _ => panic!("expected p2p signal to be intercepted"),
        }

        // Anonymous packets on the P2P topic must be rejected.
        assert!(matches!(
            route_data_packet(
                DataPacketKind::Reliable,
                Some(P2P_SIGNAL_TOPIC.to_owned()),
                None,
                payload.clone(),
            ),
            DataRoute::Ignore
        ));

        // Lossy packets on the P2P topic must be ignored.
        assert!(matches!(
            route_data_packet(
                DataPacketKind::Lossy,
                Some(P2P_SIGNAL_TOPIC.to_owned()),
                Some("peer-a".to_owned()),
                payload,
            ),
            DataRoute::Ignore
        ));

        // Malformed or oversized P2P packets must be dropped instead of reaching chat.
        assert!(matches!(
            route_data_packet(
                DataPacketKind::Reliable,
                Some(P2P_SIGNAL_TOPIC.to_owned()),
                Some("peer-a".to_owned()),
                b"not json".to_vec(),
            ),
            DataRoute::Ignore
        ));
    }

    #[test]
    fn non_p2p_topics_still_reach_session_as_backend_events() {
        match route_data_packet(
            DataPacketKind::Reliable,
            Some("chat".to_owned()),
            Some("peer-a".to_owned()),
            b"hello".to_vec(),
        ) {
            DataRoute::Backend(BackendEvent::DataReceived {
                sender,
                topic,
                payload,
            }) => {
                assert_eq!(sender.as_deref(), Some("peer-a"));
                assert_eq!(topic, "chat");
                assert_eq!(payload, b"hello");
            }
            _ => panic!("expected regular topic to forward to session"),
        }
    }
}
