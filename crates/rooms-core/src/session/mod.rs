//! Loop principal da sessão e despacho de comandos e eventos do backend.

mod admin;
mod audio;
mod avatar;
mod chat;
mod connection;
mod files;
mod rooms;
mod share;
#[cfg(test)]
mod test_support;

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::Sleep;

use crate::api::{Command, Event, SessionConfig};
use crate::backend::{BackendEvent, MediaBackend};
use crate::backoff::ReconnectPolicy;
use crate::chat::{self as chat_codec, CHAT_DELETE_TOPIC, CHAT_TOPIC};
use crate::handle::EventSink;
use crate::participant_volume::ParticipantVolumes;
use crate::token::TokenClient;

use self::admin::{AdminSession, is_server_admin_topic};
use self::avatar::AvatarTaskEvent;
use self::connection::Phase;
use self::rooms::wait_optional;
use self::share::{ThumbnailJob, ThumbnailReady};

struct Session {
    config: SessionConfig,
    backend: Arc<dyn MediaBackend>,
    tokens: Arc<dyn TokenClient>,
    sink: EventSink,
    policy: ReconnectPolicy,
    phase: Phase,
    admin: Option<AdminSession>,
    rooms_refresh_sleep: Option<Pin<Box<Sleep>>>,
    muted: bool,
    deafened: bool,
    sharing: bool,
    watching: Option<String>,
    participant_volumes: ParticipantVolumes,
    capture_targets_seq: u64,
    thumbnail_tx: UnboundedSender<ThumbnailReady>,
    thumbnail_job: Option<ThumbnailJob>,
    avatar_op_seq: u64,
    avatar_tx: UnboundedSender<AvatarTaskEvent>,
    avatar_downloads: HashSet<(String, String)>,
}

pub(crate) async fn run(
    config: SessionConfig,
    backend: Arc<dyn MediaBackend>,
    tokens: Arc<dyn TokenClient>,
    mut commands: UnboundedReceiver<Command>,
    sink: EventSink,
    policy: ReconnectPolicy,
) {
    let (thumbnail_tx, mut thumbnail_rx) = mpsc::unbounded_channel();
    let (avatar_tx, mut avatar_rx) = mpsc::unbounded_channel();
    let mut session = Session {
        config,
        backend,
        tokens,
        sink,
        policy,
        phase: Phase::Idle,
        admin: None,
        rooms_refresh_sleep: None,
        muted: false,
        deafened: false,
        sharing: false,
        watching: None,
        participant_volumes: ParticipantVolumes::default(),
        capture_targets_seq: 0,
        thumbnail_tx,
        thumbnail_job: None,
        avatar_op_seq: 0,
        avatar_tx,
        avatar_downloads: HashSet::new(),
    };
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => session.on_command(command).await,
                None => break,
            },
            input = session.phase.wait() => session.on_input(input).await,
            Some(thumb) = thumbnail_rx.recv() => session.on_thumbnail(thumb),
            Some(avatar_event) = avatar_rx.recv() => session.on_avatar_task_event(avatar_event).await,
            _ = wait_optional(&mut session.rooms_refresh_sleep) => {
                session.rooms_refresh_sleep = None;
                session.refresh_rooms();
            }
        }
    }
    session.teardown().await;
}

impl Session {
    fn emit(&self, event: Event) {
        self.sink.emit(event);
    }

    fn connected(&self) -> bool {
        matches!(self.phase, Phase::Connected { .. })
    }

    async fn on_command(&mut self, command: Command) {
        match command {
            Command::Join { room } => self.join(room).await,
            Command::Leave => {
                if self.teardown().await {
                    self.emit(Event::Left);
                }
            }
            Command::SetMuted(muted) => self.set_muted(muted).await,
            Command::SetDeafened(deafened) => self.set_deafened(deafened).await,
            Command::SetDisplayName { name } => self.set_display_name(name).await,
            Command::SetAvatar { path } => self.set_avatar(path),
            Command::ClearAvatar => self.clear_avatar(),
            Command::SendChat { text, save } => self.send_chat(&text, save).await,
            Command::LoadChatHistory { before } => self.load_chat_history(before).await,
            Command::DeleteChat { id } => self.delete_chat(&id).await,
            Command::SendFile { path } => self.send_file(path).await,
            Command::RefreshRooms => self.refresh_rooms(),
            Command::RefreshAudioDevices => self.publish_audio_devices().await,
            Command::SetInputDevice(id) => self.set_input_device(id).await,
            Command::SetOutputDevice(id) => self.set_output_device(id).await,
            Command::SetNoiseSuppression(mode) => self.set_noise_suppression(mode).await,
            Command::SetStreamVolume(percent) => self.set_stream_volume(percent).await,
            Command::SetParticipantVolume { identity, percent } => {
                self.set_participant_volume(identity, percent).await;
            }
            Command::SetNoiseGate(threshold) => self.set_noise_gate(threshold).await,
            Command::RefreshCaptureTargets => self.refresh_capture_targets().await,
            Command::CancelCaptureTargets => self.cancel_capture_targets(),
            Command::StartScreenShare { target_id, preset } => {
                self.start_share(&target_id, preset).await;
            }
            Command::StopScreenShare => self.stop_share().await,
            Command::WatchScreenShare { identity } => self.watch(identity).await,
            Command::StopWatching => self.stop_watch().await,
            Command::SetUiVisible(visible) => self.backend.set_ui_visible(visible),
            Command::PlayCue(cue) => self.backend.play_cue(cue),
            Command::AdminLogin { password } => self.admin_login(password).await,
            Command::AdminLogout => self.admin_logout(),
            Command::Admin(action) => self.run_admin_action(action).await,
        }
    }

    async fn on_backend(&mut self, event: Option<BackendEvent>) {
        if !self.connected() {
            return;
        }
        match event {
            Some(BackendEvent::ParticipantJoined(p)) => {
                if let Some(version) = p.avatar_version.as_deref() {
                    self.queue_avatar_download(&p.identity, version);
                }
                self.emit(Event::ParticipantJoined(p));
            }
            Some(BackendEvent::ParticipantLeft { identity }) => {
                let was_watched = self.watching.as_deref() == Some(identity.as_str());
                self.emit(Event::ParticipantLeft { identity });
                if was_watched {
                    self.stop_watch().await;
                }
            }
            Some(BackendEvent::MuteChanged { identity, muted }) => {
                if let Phase::Connected { local_identity, .. } = &self.phase
                    && *local_identity == identity
                {
                    self.muted = muted;
                }
                self.emit(Event::ParticipantMuteChanged { identity, muted });
            }
            Some(BackendEvent::SpeakingChanged { identity, speaking }) => {
                self.emit(Event::ParticipantSpeakingChanged { identity, speaking });
            }
            Some(BackendEvent::NameChanged { identity, name }) => {
                self.emit(Event::ParticipantNameChanged { identity, name });
            }
            Some(BackendEvent::AvatarChanged { identity, version }) => {
                if let Some(ver) = version.as_deref() {
                    self.queue_avatar_download(&identity, ver);
                }
                self.emit(Event::ParticipantAvatarChanged { identity, version });
            }
            Some(BackendEvent::DataReceived {
                sender,
                topic,
                payload,
            }) => self.on_data_received(sender, topic, payload).await,
            Some(BackendEvent::FileTransfer(transfer)) => {
                self.emit(Event::FileTransfer(transfer));
            }
            Some(BackendEvent::MicLevel {
                level_db,
                gate_open,
            }) => self.emit(Event::MicLevel {
                level_db,
                gate_open,
            }),
            Some(BackendEvent::RemoteScreenShareStarted { identity }) => {
                self.emit(Event::RemoteScreenShareStarted { identity });
            }
            Some(BackendEvent::RemoteScreenShareStopped { identity }) => {
                let was_watched = self.watching.as_deref() == Some(identity.as_str());
                self.emit(Event::RemoteScreenShareStopped { identity });
                if was_watched {
                    self.stop_watch().await;
                }
            }
            Some(BackendEvent::WatchEnded { identity, reason }) => {
                if self.watching.as_deref() == Some(identity.as_str()) {
                    self.stop_watch().await;
                    if let Some(reason) = reason {
                        self.emit(Event::CommandFailed(SessionError::Video(reason)));
                    }
                }
            }
            Some(BackendEvent::ScreenShareEnded { reason }) => {
                if self.sharing {
                    self.emit(Event::CommandFailed(SessionError::CaptureEnded(reason)));
                    self.stop_share().await;
                }
            }
            Some(BackendEvent::Reconnecting) => {
                if let Phase::Connected { reconnecting, .. } = &mut self.phase
                    && !*reconnecting
                {
                    *reconnecting = true;
                    self.emit(Event::Reconnecting);
                }
            }
            Some(BackendEvent::Reconnected) => {
                if let Phase::Connected { reconnecting, .. } = &mut self.phase
                    && *reconnecting
                {
                    *reconnecting = false;
                    self.emit(Event::Reconnected);
                }
            }
            Some(BackendEvent::RemovedFromRoom { reason }) => {
                tracing::info!(?reason, "removed from room by server");
                self.teardown().await;
                self.emit(Event::Left);
                self.emit(Event::RemovedFromRoom { reason });
            }
            Some(BackendEvent::Disconnected { reason }) => {
                self.on_unexpected_disconnect(&reason).await;
            }
            None => {
                self.on_unexpected_disconnect("backend event stream closed")
                    .await
            }
        }
    }

    async fn on_data_received(&mut self, sender: Option<String>, topic: String, payload: Vec<u8>) {
        if is_server_admin_topic(&topic) {
            if let Some(sender) = sender {
                tracing::warn!(%topic, %sender, "ignoring server topic sent by participant");
                return;
            }
            self.on_server_data(&topic, &payload).await;
            return;
        }
        let Some(sender) = sender else {
            return;
        };
        if topic == CHAT_TOPIC
            && let Some(message) = chat_codec::parse_incoming(&sender, &payload)
        {
            self.emit(Event::ChatReceived(message));
        } else if topic == CHAT_DELETE_TOPIC
            && let Some(id) = chat_codec::parse_incoming_delete(&payload)
        {
            self.emit(Event::ChatDeleted { id, sender });
        }
    }
}

use crate::api::SessionError;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::SessionHandle;
    use crate::api::{Command, Event};
    use crate::testing::{BackendCall, FakeBackend, FakeTokenClient};

    use super::test_support::{config, device};

    #[test]
    fn audio_only_session_serves_device_commands_without_tokens() {
        let backend = FakeBackend::new();
        backend.set_audio_devices(vec![device("mic1")], Vec::new());
        let mut handle = SessionHandle::spawn_audio_only(backend).unwrap();
        handle.send(Command::RefreshAudioDevices);
        handle.send(Command::Join {
            room: "geral".to_owned(),
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut events = Vec::new();
        while std::time::Instant::now() < deadline && events.len() < 2 {
            while let Some(event) = handle.try_recv() {
                events.push(event);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::AudioDevicesUpdated(d) if d.inputs.len() == 1))
        );
        assert!(events.iter().any(|e| matches!(e, Event::CommandFailed(_))));
    }

    #[test]
    fn dropping_handle_stops_session_and_disconnects() {
        let backend = FakeBackend::new();
        let mut handle =
            SessionHandle::spawn(config(), backend.clone(), FakeTokenClient::new()).unwrap();
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woken.clone();
        handle.set_wake_callback(Box::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        handle.send(Command::Join {
            room: "geral".to_owned(),
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut joined = false;
        while std::time::Instant::now() < deadline && !joined {
            while let Some(event) = handle.try_recv() {
                joined |= matches!(event, Event::Joined { .. });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(joined);
        assert!(woken.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        drop(handle);
        assert!(!backend.is_connected());
        assert_eq!(backend.calls().last(), Some(&BackendCall::Disconnect));
    }

    #[test]
    fn send_after_session_end_does_not_panic() {
        let handle =
            SessionHandle::spawn(config(), FakeBackend::new(), FakeTokenClient::new()).unwrap();
        handle.send(Command::Leave);
        drop(handle);
    }

    #[test]
    fn config_debug_hides_invite_code() {
        assert!(!format!("{:?}", config()).contains("secret"));
    }
}
