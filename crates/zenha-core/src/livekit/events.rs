//! Tradução de eventos da sala do LiveKit para `BackendEvent`.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use livekit::prelude::{
    DataPacketKind, DisconnectReason, LocalTrack, Participant as SdkParticipant, RemoteTrack, Room,
    RoomEvent, TrackPublication, TrackSource,
};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use zenha_protocol::{AVATAR_ATTR, parse_avatar_attribute};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::api::RemovalReason;
use crate::backend::BackendEvent;
use crate::backend::support::{SpeakingTracker, display_name};
use crate::files::receive::FileReceiver;
use crate::files::FILE_TOPIC;
use crate::livekit::files;
use crate::livekit::video::Watch;

use super::Flags;
use super::audio::audio_tap;
use super::room::{
    is_screen_share, is_share_audio, remote_view, set_screen_share_subscribed, wants_audio,
};

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
            if let Some(raw) = changed_attributes.get(AVATAR_ATTR) {
                vec![BackendEvent::AvatarChanged {
                    identity: String::from(participant.identity()),
                    version: parse_avatar_attribute(raw),
                }]
            } else {
                Vec::new()
            }
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
                    flags.apply_participant_volume(
                        participant.identity().as_str(),
                        &audio.rtc_track(),
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
        } => match (kind, topic) {
            (DataPacketKind::Reliable, Some(topic)) => vec![BackendEvent::DataReceived {
                sender: participant.map(|sender| String::from(sender.identity())),
                topic,
                payload: Arc::unwrap_or_clone(payload),
            }],
            _ => Vec::new(),
        },
        RoomEvent::ByteStreamOpened {
            reader,
            topic,
            participant_identity,
        } => {
            if topic == FILE_TOPIC
                && let Some(reader) = reader.take()
            {
                files::start_receive(
                    reader,
                    String::from(participant_identity),
                    files,
                    events,
                );
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
}
