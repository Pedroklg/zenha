//! Conexão de sala do LiveKit, estado de participantes e compartilhamento de vídeo.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Once, PoisonError};
use std::time::Duration;

use livekit::prelude::{
    PlatformAudio, RemoteParticipant, RemoteTrack, RemoteTrackPublication, Room, RoomError,
    RoomOptions, TrackKind, TrackSource,
};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::api::{Participant, SessionError};
use crate::backend::{BackendEvent, ConnectedRoom, MediaBackend};
use crate::backend_options::{
    MicrophoneSource, TEST_PATTERN_SIZE, TEST_PATTERN_TARGET_ID, test_pattern_target,
};
use crate::backend_support::{display_name, sanitize_error};
use crate::capture::{FrameSource, TestPatternSource};
use crate::livekit_video::{self, DesktopCaptureSource};
use crate::share_audio::AudioScope;
use crate::video::{CaptureTarget, FrameSlot, SharePreset, VideoFrame};

use super::LiveKitBackend;
use super::audio::{
    Microphone, apply_selection, open_audio, publish_selected_microphone, publish_share_audio,
    publish_tone, stop_share_audio,
};
use super::events::pump_events;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Connection {
    pub(super) room: Arc<Room>,
    pub(super) events_tx: UnboundedSender<BackendEvent>,
    // Held so the platform audio device stays alive until the room is closed.
    pub(super) audio: Option<PlatformAudio>,
    pub(super) events: JoinHandle<()>,
    pub(super) mic: Microphone,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.events.abort();
    }
}

struct RoomGuard(Option<Arc<Room>>);

impl RoomGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for RoomGuard {
    fn drop(&mut self) {
        let Some(room) = self.0.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if timeout(CLOSE_TIMEOUT, room.close()).await.is_err() {
                        tracing::warn!("closing an abandoned room timed out");
                    }
                });
            }
            Err(_) => tracing::warn!("no runtime to close an abandoned room"),
        }
    }
}

impl LiveKitBackend {
    pub(super) async fn connect_room(
        &self,
        url: &str,
        token: &str,
    ) -> Result<ConnectedRoom, SessionError> {
        timeout(CONNECT_TIMEOUT, self.establish(url, token))
            .await
            .unwrap_or_else(|_| Err(SessionError::Connection("connection timed out".to_owned())))
    }

    pub(super) async fn current_room_and_events(
        &self,
    ) -> Option<(Arc<Room>, UnboundedSender<BackendEvent>)> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|connection| (connection.room.clone(), connection.events_tx.clone()))
    }

    pub(super) async fn stop_local_share(&self) {
        let audio = self.local_share_audio.lock().await.take();
        if let Some(audio) = audio {
            stop_share_audio(audio).await;
        }
        let share = self.local_share.lock().await.take();
        if let Some(share) = share {
            livekit_video::stop_share(share).await;
        }
    }

    async fn resolve_target(&self, target_id: &str) -> Result<CaptureTarget, SessionError> {
        let find = |targets: &[CaptureTarget]| targets.iter().find(|t| t.id == target_id).cloned();
        let cached = find(&self.targets.lock().unwrap_or_else(PoisonError::into_inner));
        if let Some(target) = cached {
            return Ok(target);
        }
        let fresh = self.capture_targets().await?;
        find(&fresh).ok_or_else(|| SessionError::TargetNotFound(target_id.to_owned()))
    }

    pub(super) async fn current_audio(&self) -> Option<PlatformAudio> {
        self.connection
            .lock()
            .await
            .as_ref()
            .and_then(|connection| connection.audio.clone())
    }

    pub(super) async fn current_room(&self) -> Option<Arc<Room>> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|connection| connection.room.clone())
    }

    pub(super) async fn shutdown(&self) {
        self.stop_local_share().await;
        self.watch.clear();
        self.flags.stop_stream();
        let connection = self.connection.lock().await.take();
        let Some(connection) = connection else {
            return;
        };
        connection.events.abort();
        match timeout(CLOSE_TIMEOUT, connection.room.close()).await {
            Ok(Ok(())) | Ok(Err(RoomError::AlreadyClosed)) => {}
            Ok(Err(err)) => tracing::warn!(%err, "closing the room failed"),
            Err(_) => tracing::warn!("closing the room timed out"),
        }
    }

    async fn establish(&self, url: &str, token: &str) -> Result<ConnectedRoom, SessionError> {
        self.shutdown().await;
        install_crypto_provider();

        let selection = self.selection();
        let audio = if self.options.is_tone() {
            None
        } else {
            open_audio(selection.noise)
        };
        let mut room_options = RoomOptions::default();
        room_options.auto_subscribe = false;
        let (room, room_events) = Room::connect(url, token, room_options)
            .await
            .map_err(|err| {
                SessionError::Connection(sanitize_error(
                    &format!("could not join the room: {err}"),
                    token,
                ))
            })?;
        let room = Arc::new(room);
        let mut guard = RoomGuard(Some(room.clone()));
        // The voice engine starts while connecting and resets both devices to the Windows
        // default *communications* endpoints, so the selection only sticks from here on.
        if let Some(audio) = &audio {
            apply_selection(audio, &selection);
        }
        self.flags.set_stream_output(selection.output.clone());

        let (tx, rx) = mpsc::unbounded_channel();
        let mic = match self.options.microphone {
            MicrophoneSource::Device => match &audio {
                Some(audio) => {
                    publish_selected_microphone(
                        &room,
                        audio,
                        &selection,
                        &tx,
                        &self.flags.noise_gate,
                    )
                    .await
                }
                None => Microphone::None,
            },
            MicrophoneSource::Tone { hz, amplitude } => publish_tone(&room, hz, amplitude)
                .await
                .map_or(Microphone::None, |task| Microphone::Tone { _task: task }),
        };
        let published = !matches!(mic, Microphone::None);
        if published && self.flags.microphone_muted.load(Ordering::SeqCst) {
            apply_microphone_mute(&room, true);
        }
        if self.flags.output_muted.load(Ordering::SeqCst) {
            apply_output_mute(&room, true);
        }

        let local = room.local_participant();
        let local_identity = String::from(local.identity());
        let mut participants = vec![Participant {
            name: display_name(local.name(), &local_identity),
            identity: local_identity.clone(),
            muted: self.flags.microphone_muted.load(Ordering::SeqCst),
            speaking: false,
        }];
        let mut remotes: Vec<Participant> = room
            .remote_participants()
            .values()
            .map(remote_view)
            .collect();
        remotes.sort_by(|a, b| a.identity.cmp(&b.identity));
        participants.extend(remotes);
        tracing::info!(
            remote_participants = participants.len() - 1,
            deafened = self.flags.output_muted.load(Ordering::SeqCst),
            microphone_muted = self.flags.microphone_muted.load(Ordering::SeqCst),
            "connected to the room"
        );

        sweep_publications(&room, &tx, &self.flags);
        let mut slot = self.connection.lock().await;
        let events = tokio::spawn(pump_events(
            room_events,
            tx.clone(),
            room.clone(),
            self.flags.clone(),
            self.watch.clone(),
            self.files.clone(),
        ));
        *slot = Some(Connection {
            room: room.clone(),
            events_tx: tx,
            audio,
            events,
            mic,
        });
        guard.disarm();

        Ok(ConnectedRoom {
            local_identity,
            participants,
            events: rx,
        })
    }

    pub(super) async fn list_capture_targets(&self) -> Result<Vec<CaptureTarget>, SessionError> {
        let mut targets = blocking(|| DesktopCaptureSource::new().list_targets()).await??;
        if self.options.test_pattern_target {
            targets.insert(0, test_pattern_target());
        }
        *self.targets.lock().unwrap_or_else(PoisonError::into_inner) = targets.clone();
        Ok(targets)
    }

    pub(super) async fn capture_target_thumbnail(
        &self,
        target: &CaptureTarget,
        max_side: u32,
        timeout: Duration,
    ) -> Result<VideoFrame, SessionError> {
        let (mut capture, capture_target) = capture_for(target.clone())?;
        blocking(move || capture.capture_thumbnail(&capture_target, max_side, timeout)).await?
    }

    pub(super) async fn start_local_screen_share(
        &self,
        target_id: &str,
        preset: SharePreset,
        preview: FrameSlot,
    ) -> Result<(), SessionError> {
        let (room, events) = self
            .current_room_and_events()
            .await
            .ok_or(SessionError::NotInRoom)?;
        let mut slot = self.local_share.lock().await;
        if slot.is_some() {
            return Err(SessionError::ShareActive);
        }
        let target = self.resolve_target(target_id).await?;
        let (capture, capture_target) = capture_for(target.clone())?;
        let share = livekit_video::publish_share(
            room.clone(),
            events,
            capture,
            &capture_target,
            preset,
            preview,
            Arc::clone(&self.flags.ui_visible.0),
        )
        .await?;
        self.flags.share_sids.set_video(&share.sid_handle());
        *slot = Some(share);
        // The picture is what was asked for; the sound is best effort.
        if let Some(scope) = AudioScope::for_target(&target) {
            let audio = publish_share_audio(room, scope).await;
            if let Some(audio) = &audio {
                self.flags.share_sids.set_audio(&audio.sid);
            }
            *self.local_share_audio.lock().await = audio;
        }
        Ok(())
    }

    pub(super) async fn watch_remote_screen_share(
        &self,
        identity: &str,
        frames: FrameSlot,
    ) -> Result<(), SessionError> {
        let (room, events) = self
            .current_room_and_events()
            .await
            .ok_or(SessionError::NotInRoom)?;
        let participant = find_participant(&room, identity)
            .ok_or_else(|| SessionError::TargetNotFound(identity.to_owned()))?;
        let publication = participant
            .track_publications()
            .into_values()
            .find(is_screen_share)
            .ok_or_else(|| {
                SessionError::Video("the participant is not sharing their screen".to_owned())
            })?;
        let previous = self.watch.replace(identity, &publication.sid(), frames);
        if let Some(previous) = previous.filter(|previous| previous != identity) {
            set_screen_share_subscribed(&room, &previous, false);
        }
        publication.set_subscribed(true);
        for sound in participant.track_publications().into_values() {
            if is_share_audio(&sound) {
                sound.set_subscribed(true);
            }
        }
        if let Some(RemoteTrack::Video(video)) = publication.track() {
            self.watch.start_stream(
                identity,
                video.rtc_track(),
                events,
                Arc::clone(&self.flags.ui_visible.0),
            );
        }
        Ok(())
    }

    pub(super) async fn stop_watching_share(&self) -> Result<(), SessionError> {
        self.flags.stop_stream();
        if let Some(identity) = self.watch.clear()
            && let Some(room) = self.current_room().await
        {
            set_screen_share_subscribed(&room, &identity, false);
        }
        Ok(())
    }
}

fn capture_for(
    target: CaptureTarget,
) -> Result<(Box<dyn FrameSource>, CaptureTarget), SessionError> {
    if target.id != TEST_PATTERN_TARGET_ID {
        return Ok((Box::new(DesktopCaptureSource::new()), target));
    }
    let (width, height) = TEST_PATTERN_SIZE;
    let mut source = TestPatternSource::new(width, height);
    let inner = source
        .list_targets()?
        .into_iter()
        .next()
        .ok_or_else(|| SessionError::Internal("test pattern has no target".to_owned()))?;
    Ok((Box::new(source), inner))
}

pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SessionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| SessionError::Internal(format!("background task failed: {err}")))
}

fn install_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

pub(super) fn apply_microphone_mute(room: &Room, muted: bool) {
    for publication in room.local_participant().track_publications().into_values() {
        if publication.source() != TrackSource::Microphone {
            continue;
        }
        if muted {
            publication.mute();
        } else {
            publication.unmute();
        }
    }
}

pub(super) fn apply_output_mute(room: &Room, muted: bool) {
    for participant in room.remote_participants().into_values() {
        for publication in participant.track_publications().into_values() {
            if publication.kind() == TrackKind::Audio {
                publication.set_enabled(!muted);
            }
        }
    }
}

pub(super) fn is_share_audio(publication: &RemoteTrackPublication) -> bool {
    publication.kind() == TrackKind::Audio && publication.source() == TrackSource::ScreenshareAudio
}

pub(super) fn is_screen_share(publication: &RemoteTrackPublication) -> bool {
    publication.kind() == TrackKind::Video && publication.source() == TrackSource::Screenshare
}

pub(super) fn wants_audio(publication: &RemoteTrackPublication) -> bool {
    publication.kind() == TrackKind::Audio && publication.source() != TrackSource::ScreenshareAudio
}

fn find_participant(room: &Room, identity: &str) -> Option<RemoteParticipant> {
    room.remote_participants()
        .into_values()
        .find(|participant| participant.identity().as_str() == identity)
}

pub(super) fn apply_participant_volume(room: &Room, identity: &str, gain: f64) -> usize {
    let Some(participant) = find_participant(room, identity) else {
        return 0;
    };
    let mut applied = 0;
    for publication in participant.track_publications().into_values() {
        if wants_audio(&publication)
            && let Some(RemoteTrack::Audio(audio)) = publication.track()
        {
            audio.rtc_track().set_volume(gain);
            applied += 1;
        }
    }
    applied
}

pub(super) fn set_screen_share_subscribed(room: &Room, identity: &str, subscribed: bool) {
    let Some(participant) = find_participant(room, identity) else {
        return;
    };
    for publication in participant.track_publications().into_values() {
        if is_screen_share(&publication) || is_share_audio(&publication) {
            publication.set_subscribed(subscribed);
        }
    }
}

/// The room is joined with auto-subscribe off: audio is subscribed explicitly here and
/// on every later publication, video only when the user chooses to watch it.
fn sweep_publications(room: &Room, events: &UnboundedSender<BackendEvent>, flags: &super::Flags) {
    for participant in room.remote_participants().into_values() {
        let identity = String::from(participant.identity());
        for publication in participant.track_publications().into_values() {
            if wants_audio(&publication) {
                publication.set_subscribed(true);
                if let Some(RemoteTrack::Audio(audio)) = publication.track() {
                    flags.apply_participant_volume(&identity, &audio.rtc_track());
                }
            } else if is_screen_share(&publication) {
                let _ = events.send(BackendEvent::RemoteScreenShareStarted {
                    identity: identity.clone(),
                });
            }
        }
    }
}

fn microphone_muted(participant: &RemoteParticipant) -> bool {
    participant
        .track_publications()
        .values()
        .any(|publication| {
            publication.source() == TrackSource::Microphone && publication.is_muted()
        })
}

pub(super) fn remote_view(participant: &RemoteParticipant) -> Participant {
    let identity = String::from(participant.identity());
    Participant {
        name: display_name(participant.name(), &identity),
        muted: microphone_muted(participant),
        speaking: false,
        identity,
    }
}
