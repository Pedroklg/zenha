use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use rooms_protocol::{
    MAX_FILE_BYTES, TokenRequest, validate_client_id, validate_display_name, validate_room_name,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Sleep;

use crate::api::{Command, Event, FileTransfer, SessionConfig, SessionError, TransferState};
use crate::backend::{BackendEvent, ConnectedRoom, MediaBackend};
use crate::backoff::ReconnectPolicy;
use crate::chat::{self, CHAT_TOPIC};
use crate::clock::unix_ms;
use crate::files::{mime_for_name, sanitize_file_name};
use crate::handle::EventSink;
use crate::participant_volume::ParticipantVolumes;
use crate::token::TokenClient;
use crate::video::{FrameSlot, SharePreset};

const NOISE_GATE_MIN_DB: i8 = -70;
const NOISE_GATE_MAX_DB: i8 = -20;

type Attempt = Pin<Box<dyn Future<Output = Result<ConnectedRoom, SessionError>> + Send>>;

enum Phase {
    Idle,
    Connecting {
        room: String,
        retry: Option<u32>,
        attempt: Attempt,
    },
    Connected {
        room: String,
        local_identity: String,
        events: UnboundedReceiver<BackendEvent>,
        reconnecting: bool,
    },
    Backoff {
        room: String,
        attempt: u32,
        sleep: Pin<Box<Sleep>>,
    },
}

enum Input {
    Backend(Option<BackendEvent>),
    Attempt(Result<ConnectedRoom, SessionError>),
    BackoffElapsed,
}

impl Phase {
    async fn wait(&mut self) -> Input {
        match self {
            Phase::Idle => std::future::pending().await,
            Phase::Connecting { attempt, .. } => Input::Attempt(attempt.as_mut().await),
            Phase::Connected { events, .. } => Input::Backend(events.recv().await),
            Phase::Backoff { sleep, .. } => {
                sleep.as_mut().await;
                Input::BackoffElapsed
            }
        }
    }
}

struct Session {
    config: SessionConfig,
    backend: Arc<dyn MediaBackend>,
    tokens: Arc<dyn TokenClient>,
    sink: EventSink,
    policy: ReconnectPolicy,
    phase: Phase,
    muted: bool,
    deafened: bool,
    sharing: bool,
    watching: Option<String>,
    participant_volumes: ParticipantVolumes,
}

pub(crate) async fn run(
    config: SessionConfig,
    backend: Arc<dyn MediaBackend>,
    tokens: Arc<dyn TokenClient>,
    mut commands: UnboundedReceiver<Command>,
    sink: EventSink,
    policy: ReconnectPolicy,
) {
    let mut session = Session {
        config,
        backend,
        tokens,
        sink,
        policy,
        phase: Phase::Idle,
        muted: false,
        deafened: false,
        sharing: false,
        watching: None,
        participant_volumes: ParticipantVolumes::default(),
    };
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => session.on_command(command).await,
                None => break,
            },
            input = session.phase.wait() => session.on_input(input).await,
        }
    }
    session.teardown().await;
}

impl Session {
    fn emit(&self, event: Event) {
        self.sink.emit(event);
    }

    fn start_attempt(&mut self, room: String, retry: Option<u32>) {
        let request = TokenRequest {
            client_id: self.config.client_id.clone(),
            display_name: self.config.display_name.clone(),
            room: room.clone(),
            invite_code: self.config.invite_code.clone(),
        };
        let tokens = self.tokens.clone();
        let backend = self.backend.clone();
        let attempt: Attempt = Box::pin(async move {
            let token = tokens.fetch_token(request).await?;
            backend.connect(&token.livekit_url, &token.token).await
        });
        self.phase = Phase::Connecting {
            room,
            retry,
            attempt,
        };
    }

    async fn reset_video(&mut self) {
        if self.sharing {
            self.sharing = false;
            if let Err(err) = self.backend.stop_screen_share().await {
                tracing::warn!(%err, "backend stop screen share failed");
            }
            self.emit(Event::ScreenShareStopped);
        }
        if self.watching.take().is_some() {
            if let Err(err) = self.backend.stop_watching().await {
                tracing::warn!(%err, "backend stop watching failed");
            }
            self.emit(Event::WatchingStopped);
        }
    }

    async fn teardown(&mut self) -> bool {
        self.reset_video().await;
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => false,
            Phase::Connected { .. } => {
                if let Err(err) = self.backend.disconnect().await {
                    tracing::warn!(%err, "backend disconnect failed");
                }
                true
            }
            Phase::Connecting { .. } | Phase::Backoff { .. } => true,
        }
    }

    async fn on_command(&mut self, command: Command) {
        match command {
            Command::Join { room } => self.join(room).await,
            Command::Leave => {
                if self.teardown().await {
                    self.emit(Event::Left);
                }
            }
            Command::SetMuted(muted) => {
                self.muted = muted;
                if matches!(self.phase, Phase::Connected { .. })
                    && let Err(err) = self.backend.set_microphone_muted(muted).await
                {
                    self.emit(Event::CommandFailed(err));
                }
            }
            Command::SetDeafened(deafened) => {
                self.deafened = deafened;
                if matches!(self.phase, Phase::Connected { .. })
                    && let Err(err) = self.backend.set_output_muted(deafened).await
                {
                    self.emit(Event::CommandFailed(err));
                }
            }
            Command::SendChat { text } => self.send_chat(&text).await,
            Command::SendFile { path } => self.send_file(path).await,
            Command::RefreshRooms => self.refresh_rooms(),
            Command::RefreshAudioDevices => self.publish_audio_devices().await,
            Command::SetInputDevice(id) => match self.backend.set_input_device(id).await {
                Ok(()) => self.publish_audio_devices().await,
                Err(err) => self.emit(Event::CommandFailed(err)),
            },
            Command::SetOutputDevice(id) => match self.backend.set_output_device(id).await {
                Ok(()) => self.publish_audio_devices().await,
                Err(err) => self.emit(Event::CommandFailed(err)),
            },
            Command::SetNoiseSuppression(mode) => {
                if let Err(err) = self.backend.set_noise_suppression(mode).await {
                    self.emit(Event::CommandFailed(err));
                }
            }
            Command::SetStreamVolume(percent) => {
                if let Err(err) = self.backend.set_stream_volume(percent.min(100)).await {
                    self.emit(Event::CommandFailed(err));
                }
            }
            Command::SetParticipantVolume { identity, percent } => {
                let clamped = self.participant_volumes.set(&identity, percent);
                if let Err(err) = self
                    .backend
                    .set_participant_volume(&identity, clamped)
                    .await
                {
                    self.emit(Event::CommandFailed(err));
                }
            }
            Command::SetNoiseGate(threshold) => {
                let threshold = threshold.map(|db| db.clamp(NOISE_GATE_MIN_DB, NOISE_GATE_MAX_DB));
                if let Err(err) = self.backend.set_noise_gate(threshold).await {
                    self.emit(Event::CommandFailed(err));
                }
            }
            Command::RefreshCaptureTargets => match self.backend.capture_targets().await {
                Ok(targets) => self.emit(Event::CaptureTargetsUpdated(targets)),
                Err(err) => self.emit(Event::CommandFailed(err)),
            },
            Command::StartScreenShare { target_id, preset } => {
                self.start_share(&target_id, preset).await;
            }
            Command::StopScreenShare => self.stop_share().await,
            Command::WatchScreenShare { identity } => self.watch(identity).await,
            Command::StopWatching => self.stop_watch().await,
            Command::PlayCue(cue) => self.backend.play_cue(cue),
        }
    }

    fn connected(&self) -> bool {
        matches!(self.phase, Phase::Connected { .. })
    }

    async fn start_share(&mut self, target_id: &str, preset: SharePreset) {
        if !self.connected() {
            self.emit(Event::CommandFailed(SessionError::NotInRoom));
            return;
        }
        if self.sharing {
            self.emit(Event::CommandFailed(SessionError::ShareActive));
            return;
        }
        match self.backend.start_screen_share(target_id, preset).await {
            Ok(()) => {
                self.sharing = true;
                self.emit(Event::ScreenShareStarted);
            }
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    async fn stop_share(&mut self) {
        if !self.sharing {
            return;
        }
        self.sharing = false;
        if let Err(err) = self.backend.stop_screen_share().await {
            self.emit(Event::CommandFailed(err));
        }
        self.emit(Event::ScreenShareStopped);
    }

    async fn watch(&mut self, identity: String) {
        if !self.connected() {
            self.emit(Event::CommandFailed(SessionError::NotInRoom));
            return;
        }
        if self.watching.as_deref() == Some(identity.as_str()) {
            return;
        }
        let frames = FrameSlot::with_waker(self.sink.waker());
        match self
            .backend
            .watch_screen_share(&identity, frames.clone())
            .await
        {
            Ok(()) => {
                self.watching = Some(identity.clone());
                self.emit(Event::WatchingStarted { identity, frames });
            }
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    async fn stop_watch(&mut self) {
        if self.watching.take().is_none() {
            return;
        }
        if let Err(err) = self.backend.stop_watching().await {
            self.emit(Event::CommandFailed(err));
        }
        self.emit(Event::WatchingStopped);
    }

    async fn publish_audio_devices(&mut self) {
        match self.backend.audio_devices().await {
            Ok(devices) => self.emit(Event::AudioDevicesUpdated(devices)),
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    async fn join(&mut self, room: String) {
        let valid = validate_room_name(&room)
            .and_then(|()| validate_display_name(&self.config.display_name))
            .and_then(|()| validate_client_id(&self.config.client_id));
        if let Err(err) = valid {
            self.emit(Event::CommandFailed(SessionError::InvalidInput(
                err.to_string(),
            )));
            return;
        }
        if self.teardown().await {
            self.emit(Event::Left);
        }
        self.emit(Event::Connecting);
        self.start_attempt(room, None);
    }

    async fn send_chat(&mut self, text: &str) {
        let Phase::Connected { local_identity, .. } = &self.phase else {
            self.emit(Event::CommandFailed(SessionError::NotInRoom));
            return;
        };
        let id = uuid::Uuid::new_v4().to_string();
        let (message, payload) = match chat::build_outgoing(id, local_identity, text, unix_ms()) {
            Ok(built) => built,
            Err(err) => {
                self.emit(Event::CommandFailed(err));
                return;
            }
        };
        match self.backend.send_data(CHAT_TOPIC, payload).await {
            Ok(()) => self.emit(Event::ChatReceived(message)),
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    async fn send_file(&mut self, path: PathBuf) {
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

    fn refresh_rooms(&self) {
        let tokens = self.tokens.clone();
        let sink = self.sink.clone();
        let invite_code = self.config.invite_code.clone();
        tokio::spawn(async move {
            match tokens.fetch_rooms(&invite_code).await {
                Ok(rooms) => sink.emit(Event::RoomsUpdated(rooms)),
                Err(err) => sink.emit(Event::CommandFailed(err)),
            }
        });
    }

    async fn on_input(&mut self, input: Input) {
        match input {
            Input::Attempt(result) => self.on_attempt(result).await,
            Input::Backend(event) => self.on_backend(event).await,
            Input::BackoffElapsed => {
                if let Phase::Backoff { room, attempt, .. } =
                    std::mem::replace(&mut self.phase, Phase::Idle)
                {
                    self.start_attempt(room, Some(attempt));
                }
            }
        }
    }

    async fn on_attempt(&mut self, result: Result<ConnectedRoom, SessionError>) {
        let Phase::Connecting { room, retry, .. } = std::mem::replace(&mut self.phase, Phase::Idle)
        else {
            return;
        };
        match result {
            Ok(connected) => self.on_connected(room, retry, connected).await,
            Err(err) => match retry {
                Some(attempt)
                    if attempt < self.policy.max_attempts
                        && err != SessionError::InviteRejected =>
                {
                    self.schedule_retry(room, attempt + 1);
                }
                _ => self.emit(Event::Failed(err)),
            },
        }
    }

    async fn on_connected(&mut self, room: String, retry: Option<u32>, connected: ConnectedRoom) {
        let ConnectedRoom {
            local_identity,
            participants,
            events,
        } = connected;
        if let Err(err) = self.backend.set_microphone_muted(self.muted).await {
            tracing::warn!(%err, "could not apply microphone state");
        }
        if let Err(err) = self.backend.set_output_muted(self.deafened).await {
            tracing::warn!(%err, "could not apply output state");
        }
        let volumes: Vec<(String, u16)> = self
            .participant_volumes
            .iter()
            .map(|(identity, percent)| (identity.to_owned(), percent))
            .collect();
        for (identity, percent) in volumes {
            if let Err(err) = self
                .backend
                .set_participant_volume(&identity, percent)
                .await
            {
                tracing::warn!(%err, identity, "could not apply participant volume");
            }
        }
        self.phase = Phase::Connected {
            room: room.clone(),
            local_identity: local_identity.clone(),
            events,
            reconnecting: false,
        };
        if retry.is_some() {
            self.emit(Event::Reconnected);
            self.emit(Event::ParticipantList(participants));
        } else {
            self.emit(Event::Joined {
                room,
                local_identity,
                participants,
            });
        }
    }

    fn schedule_retry(&mut self, room: String, attempt: u32) {
        let sleep = Box::pin(tokio::time::sleep(self.policy.delay(attempt)));
        self.phase = Phase::Backoff {
            room,
            attempt,
            sleep,
        };
    }

    async fn on_backend(&mut self, event: Option<BackendEvent>) {
        if !self.connected() {
            return;
        }
        match event {
            Some(BackendEvent::ParticipantJoined(p)) => self.emit(Event::ParticipantJoined(p)),
            Some(BackendEvent::ParticipantLeft { identity }) => {
                let was_watched = self.watching.as_deref() == Some(identity.as_str());
                self.emit(Event::ParticipantLeft { identity });
                if was_watched {
                    self.stop_watch().await;
                }
            }
            Some(BackendEvent::MuteChanged { identity, muted }) => {
                self.emit(Event::ParticipantMuteChanged { identity, muted });
            }
            Some(BackendEvent::SpeakingChanged { identity, speaking }) => {
                self.emit(Event::ParticipantSpeakingChanged { identity, speaking });
            }
            Some(BackendEvent::DataReceived {
                sender,
                topic,
                payload,
            }) => {
                if topic == CHAT_TOPIC
                    && let Some(message) = chat::parse_incoming(&sender, &payload)
                {
                    self.emit(Event::ChatReceived(message));
                }
            }
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
            Some(BackendEvent::Disconnected { reason }) => {
                self.on_unexpected_disconnect(&reason).await;
            }
            None => {
                self.on_unexpected_disconnect("backend event stream closed")
                    .await
            }
        }
    }

    async fn on_unexpected_disconnect(&mut self, reason: &str) {
        tracing::warn!(reason, "unexpected disconnect, starting reconnect");
        self.reset_video().await;
        if let Phase::Connected {
            room, reconnecting, ..
        } = std::mem::replace(&mut self.phase, Phase::Idle)
        {
            if !reconnecting {
                self.emit(Event::Reconnecting);
            }
            self.schedule_retry(room, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rooms_protocol::{ChatMessage, RoomSummary};
    use tokio::sync::mpsc;
    use tokio::time::Instant;

    use super::*;
    use crate::api::{AudioDevice, AudioDevices, Participant};
    use crate::capture::CaptureEnd;
    use crate::cue::Cue;
    use crate::testing::{BackendCall, FakeBackend, FakeTokenClient};
    use crate::video::{CaptureKind, CaptureTarget, VideoFrame};

    struct Harness {
        commands: mpsc::UnboundedSender<Command>,
        events: mpsc::UnboundedReceiver<Event>,
        backend: FakeBackend,
        tokens: FakeTokenClient,
    }

    fn config() -> SessionConfig {
        SessionConfig {
            token_server_url: "http://unused.invalid".to_owned(),
            client_id: "client-pedro".to_owned(),
            display_name: "pedro".to_owned(),
            invite_code: "secret".to_owned(),
        }
    }

    fn participant(identity: &str) -> Participant {
        Participant {
            identity: identity.to_owned(),
            name: identity.to_uppercase(),
            muted: false,
            speaking: false,
        }
    }

    fn harness() -> Harness {
        let backend = FakeBackend::new();
        let tokens = FakeTokenClient::new();
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        tokio::spawn(run(
            config(),
            Arc::new(backend.clone()),
            Arc::new(tokens.clone()),
            command_rx,
            EventSink::new(event_tx),
            ReconnectPolicy::default(),
        ));
        Harness {
            commands: command_tx,
            events: event_rx,
            backend,
            tokens,
        }
    }

    impl Harness {
        fn send(&self, command: Command) {
            self.commands.send(command).unwrap();
        }

        async fn next(&mut self) -> Event {
            self.events.recv().await.unwrap()
        }

        async fn join(&mut self, room: &str) {
            self.send(Command::Join {
                room: room.to_owned(),
            });
            assert_eq!(self.next().await, Event::Connecting);
            assert!(matches!(self.next().await, Event::Joined { .. }));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn join_emits_connecting_then_joined_and_forwards_participant_changes() {
        let mut h = harness();
        h.backend.set_participants(vec![participant("a")]);
        h.send(Command::Join {
            room: "geral".to_owned(),
        });
        assert_eq!(h.next().await, Event::Connecting);
        assert_eq!(
            h.next().await,
            Event::Joined {
                room: "geral".to_owned(),
                local_identity: "local".to_owned(),
                participants: vec![participant("a")],
            }
        );
        assert_eq!(h.tokens.requests()[0].client_id, "client-pedro");
        assert_eq!(h.tokens.requests()[0].invite_code, "secret");

        h.backend
            .emit(BackendEvent::ParticipantJoined(participant("b")));
        h.backend.emit(BackendEvent::MuteChanged {
            identity: "b".to_owned(),
            muted: true,
        });
        h.backend.emit(BackendEvent::SpeakingChanged {
            identity: "b".to_owned(),
            speaking: true,
        });
        h.backend.emit(BackendEvent::ParticipantLeft {
            identity: "a".to_owned(),
        });
        assert_eq!(h.next().await, Event::ParticipantJoined(participant("b")));
        assert_eq!(
            h.next().await,
            Event::ParticipantMuteChanged {
                identity: "b".to_owned(),
                muted: true
            }
        );
        assert_eq!(
            h.next().await,
            Event::ParticipantSpeakingChanged {
                identity: "b".to_owned(),
                speaking: true
            }
        );
        assert_eq!(
            h.next().await,
            Event::ParticipantLeft {
                identity: "a".to_owned()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn invite_rejected_fails_and_later_join_works() {
        let mut h = harness();
        h.tokens.set_reject_invite(true);
        h.send(Command::Join {
            room: "geral".to_owned(),
        });
        assert_eq!(h.next().await, Event::Connecting);
        assert_eq!(h.next().await, Event::Failed(SessionError::InviteRejected));
        assert!(!h.backend.is_connected());

        h.tokens.set_reject_invite(false);
        h.join("geral").await;
        assert!(h.backend.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn invalid_room_name_is_rejected_without_connecting() {
        let mut h = harness();
        h.send(Command::Join {
            room: "Bad Room".to_owned(),
        });
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        assert_eq!(h.backend.connect_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn invalid_client_id_is_rejected_without_connecting() {
        let backend = FakeBackend::new();
        let tokens = FakeTokenClient::new();
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut bad_config = config();
        bad_config.client_id = "Bad_Id".to_owned();
        tokio::spawn(run(
            bad_config,
            Arc::new(backend.clone()),
            Arc::new(tokens),
            command_rx,
            EventSink::new(event_tx),
            ReconnectPolicy::default(),
        ));
        command_tx
            .send(Command::Join {
                room: "geral".to_owned(),
            })
            .unwrap();
        assert!(matches!(
            event_rx.recv().await.unwrap(),
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        assert_eq!(backend.connect_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn mute_and_deafen_reach_backend_and_survive_room_switch() {
        let mut h = harness();
        h.send(Command::SetMuted(true));
        h.join("geral").await;
        h.send(Command::SetDeafened(true));
        h.send(Command::Join {
            room: "jogos".to_owned(),
        });
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { room, .. } if room == "jogos"));

        let calls = h.backend.calls();
        assert_eq!(
            &calls[calls.len() - 4..],
            [
                BackendCall::Disconnect,
                BackendCall::Connect {
                    url: "ws://fake.invalid".to_owned(),
                    token: "token-jogos-2".to_owned()
                },
                BackendCall::SetMicrophoneMuted(true),
                BackendCall::SetOutputMuted(true),
            ]
        );
        assert!(calls.contains(&BackendCall::SetOutputMuted(true)));
    }

    #[tokio::test(start_paused = true)]
    async fn outgoing_chat_reaches_backend_and_is_echoed() {
        let mut h = harness();
        h.join("geral").await;
        h.send(Command::SendChat {
            text: "oi".to_owned(),
        });
        let Event::ChatReceived(echo) = h.next().await else {
            panic!("expected echo");
        };
        assert_eq!(echo.sender, "local");
        assert_eq!(echo.text, "oi");
        assert!(echo.sent_at_unix_ms > 0);

        let sent = h.backend.calls().into_iter().find_map(|call| match call {
            BackendCall::SendData { topic, payload } => Some((topic, payload)),
            _ => None,
        });
        let (topic, payload) = sent.unwrap();
        assert_eq!(topic, "chat");
        assert_eq!(
            serde_json::from_slice::<ChatMessage>(&payload).unwrap(),
            echo
        );
    }

    #[tokio::test(start_paused = true)]
    async fn chat_outside_a_room_or_invalid_is_rejected() {
        let mut h = harness();
        h.send(Command::SendChat {
            text: "oi".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::NotInRoom)
        );
        h.join("geral").await;
        h.send(Command::SendChat {
            text: "   ".to_owned(),
        });
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
    }

    fn data(topic: &str, payload: Vec<u8>) -> BackendEvent {
        BackendEvent::DataReceived {
            sender: "ana".to_owned(),
            topic: topic.to_owned(),
            payload,
        }
    }

    fn chat_json(sender: &str, text: &str) -> Vec<u8> {
        serde_json::to_vec(&ChatMessage {
            id: "m1".to_owned(),
            sender: sender.to_owned(),
            sent_at_unix_ms: 9,
            text: text.to_owned(),
            saved: false,
        })
        .unwrap()
    }

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

    #[tokio::test(start_paused = true)]
    async fn incoming_chat_uses_backend_identity_and_drops_bad_payloads() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.emit(data("chat", b"{broken".to_vec()));
        h.backend
            .emit(data("chat", chat_json("ana", &"x".repeat(2001))));
        h.backend.emit(data("other", chat_json("ana", "ignored")));
        h.backend.emit(data("chat", chat_json("spoofed", "olá")));
        let Event::ChatReceived(message) = h.next().await else {
            panic!("expected chat");
        };
        assert_eq!(message.sender, "ana");
        assert_eq!(message.text, "olá");
        assert!(h.events.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn unexpected_disconnect_reconnects_with_backoff() {
        let mut h = harness();
        h.join("geral").await;
        h.send(Command::SetMuted(true));
        h.backend.fail_next_connects(2);
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert!(matches!(h.next().await, Event::ParticipantList(_)));

        let instants = h.backend.connect_instants();
        let gaps: Vec<Duration> = instants.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(
            gaps,
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4)
            ]
        );
        assert_eq!(h.tokens.requests().len(), 4);
        assert!(h.backend.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn exhausting_attempts_ends_in_failed() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.fail_next_connects(100);
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        assert!(matches!(
            h.next().await,
            Event::Failed(SessionError::Connection(_))
        ));

        let policy = ReconnectPolicy::default();
        let instants = h.backend.connect_instants();
        assert_eq!(instants.len(), 1 + policy.max_attempts as usize);
        let gaps: Vec<Duration> = instants.windows(2).map(|w| w[1] - w[0]).collect();
        let expected: Vec<Duration> = (1..=policy.max_attempts).map(|n| policy.delay(n)).collect();
        assert_eq!(gaps, expected);

        h.backend.fail_next_connects(0);
        h.join("geral").await;
    }

    #[tokio::test(start_paused = true)]
    async fn leave_during_backoff_stops_retries() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        h.send(Command::Leave);
        assert_eq!(h.next().await, Event::Left);

        let start = Instant::now();
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert!(Instant::now() - start >= Duration::from_secs(120));
        assert_eq!(h.backend.connect_count(), 1);
        assert!(h.events.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn backend_reconnect_events_are_forwarded_once() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.emit(BackendEvent::Reconnecting);
        h.backend.emit(BackendEvent::Reconnecting);
        h.backend.emit(BackendEvent::Reconnected);
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert!(h.events.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_rooms_emits_rooms_updated() {
        let mut h = harness();
        let rooms = vec![RoomSummary {
            name: "geral".to_owned(),
            participants: vec!["ana".to_owned()],
        }];
        h.tokens.set_rooms(rooms.clone());
        h.send(Command::RefreshRooms);
        assert_eq!(h.next().await, Event::RoomsUpdated(rooms));
    }

    fn device(id: &str) -> AudioDevice {
        AudioDevice {
            id: id.to_owned(),
            name: id.to_uppercase(),
        }
    }

    fn with_devices(h: &Harness) {
        h.backend
            .set_audio_devices(vec![device("mic1"), device("mic2")], vec![device("out1")]);
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_audio_devices_emits_the_list_outside_a_room() {
        let mut h = harness();
        with_devices(&h);
        h.send(Command::RefreshAudioDevices);
        assert_eq!(
            h.next().await,
            Event::AudioDevicesUpdated(AudioDevices {
                inputs: vec![device("mic1"), device("mic2")],
                outputs: vec![device("out1")],
                selected_input: None,
                selected_output: None,
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn devices_chosen_before_join_are_applied_on_connect() {
        let mut h = harness();
        with_devices(&h);
        h.send(Command::SetInputDevice(Some("mic2".to_owned())));
        h.send(Command::SetOutputDevice(Some("out1".to_owned())));
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        h.join("geral").await;
        assert_eq!(
            h.backend.applied_devices(),
            vec![(Some("mic2".to_owned()), Some("out1".to_owned()))]
        );
        let calls = h.backend.calls();
        let connect = calls
            .iter()
            .position(|c| matches!(c, BackendCall::Connect { .. }));
        let set = calls
            .iter()
            .position(|c| matches!(c, BackendCall::SetOutputDevice(_)));
        assert!(set < connect);
    }

    #[tokio::test(start_paused = true)]
    async fn device_change_in_a_room_reaches_the_backend_and_emits_the_update() {
        let mut h = harness();
        with_devices(&h);
        h.join("geral").await;
        h.send(Command::SetInputDevice(Some("mic1".to_owned())));
        let Event::AudioDevicesUpdated(devices) = h.next().await else {
            panic!("expected AudioDevicesUpdated");
        };
        assert_eq!(devices.selected_input.as_deref(), Some("mic1"));
        assert!(
            h.backend
                .calls()
                .contains(&BackendCall::SetInputDevice(Some("mic1".to_owned())))
        );
        assert!(h.backend.is_connected());

        h.send(Command::SetInputDevice(None));
        let Event::AudioDevicesUpdated(devices) = h.next().await else {
            panic!("expected AudioDevicesUpdated");
        };
        assert_eq!(devices.selected_input, None);
    }

    #[tokio::test(start_paused = true)]
    async fn device_choice_survives_room_switch_and_reconnect() {
        let mut h = harness();
        with_devices(&h);
        h.send(Command::SetOutputDevice(Some("out1".to_owned())));
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        h.join("geral").await;
        h.send(Command::Join {
            room: "jogos".to_owned(),
        });
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { .. }));
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert!(matches!(h.next().await, Event::ParticipantList(_)));
        let applied = h.backend.applied_devices();
        assert_eq!(applied.len(), 3);
        assert!(
            applied
                .iter()
                .all(|(input, output)| input.is_none() && output.as_deref() == Some("out1"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_device_falls_back_to_default_without_failing_the_join() {
        let mut h = harness();
        with_devices(&h);
        h.send(Command::SetInputDevice(Some("unplugged".to_owned())));
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        h.join("geral").await;
        assert_eq!(h.backend.applied_devices(), vec![(None, None)]);
        assert!(h.backend.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn backend_error_on_device_change_becomes_command_failed() {
        let mut h = harness();
        with_devices(&h);
        h.backend
            .fail_next_device_change(SessionError::Internal("boom".to_owned()));
        h.send(Command::SetOutputDevice(Some("out1".to_owned())));
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::Internal("boom".to_owned()))
        );
        assert!(h.events.try_recv().is_err());

        h.send(Command::RefreshAudioDevices);
        let Event::AudioDevicesUpdated(devices) = h.next().await else {
            panic!("expected AudioDevicesUpdated");
        };
        assert_eq!(devices.selected_output, None);
    }

    fn target(id: &str) -> CaptureTarget {
        CaptureTarget {
            id: id.to_owned(),
            name: id.to_uppercase(),
            kind: CaptureKind::Screen,
        }
    }

    fn start_share(h: &Harness, id: &str) {
        h.send(Command::StartScreenShare {
            target_id: id.to_owned(),
            preset: SharePreset::P720F30,
        });
    }

    #[tokio::test(start_paused = true)]
    async fn capture_targets_are_listed_even_outside_a_room() {
        let mut h = harness();
        h.backend.set_capture_targets(vec![target("s1")]);
        h.send(Command::RefreshCaptureTargets);
        assert_eq!(
            h.next().await,
            Event::CaptureTargetsUpdated(vec![target("s1")])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn start_share_reaches_the_backend_and_second_start_is_rejected() {
        let mut h = harness();
        start_share(&h, "s1");
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::NotInRoom)
        );
        h.join("geral").await;
        start_share(&h, "s1");
        assert_eq!(h.next().await, Event::ScreenShareStarted);
        assert_eq!(
            h.backend.sharing(),
            Some(("s1".to_owned(), SharePreset::P720F30))
        );
        start_share(&h, "s2");
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::ShareActive)
        );
        assert_eq!(h.backend.sharing().unwrap().0, "s1");
        h.send(Command::StopScreenShare);
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.backend.sharing(), None);
        h.send(Command::StopScreenShare);
        start_share(&h, "s2");
        assert_eq!(h.next().await, Event::ScreenShareStarted);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_start_reports_the_error_and_allows_a_retry() {
        let mut h = harness();
        h.join("geral").await;
        h.backend
            .fail_next_share(SessionError::TargetNotFound("s1".to_owned()));
        start_share(&h, "s1");
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::TargetNotFound("s1".to_owned()))
        );
        start_share(&h, "s1");
        assert_eq!(h.next().await, Event::ScreenShareStarted);
    }

    #[tokio::test(start_paused = true)]
    async fn share_stops_on_leave_and_on_room_switch() {
        let mut h = harness();
        h.join("geral").await;
        start_share(&h, "s1");
        assert_eq!(h.next().await, Event::ScreenShareStarted);
        h.send(Command::Leave);
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.next().await, Event::Left);
        let calls = h.backend.calls();
        let stop = calls
            .iter()
            .position(|c| *c == BackendCall::StopScreenShare)
            .unwrap();
        let disconnect = calls
            .iter()
            .position(|c| *c == BackendCall::Disconnect)
            .unwrap();
        assert!(stop < disconnect);

        h.join("geral").await;
        start_share(&h, "s1");
        assert_eq!(h.next().await, Event::ScreenShareStarted);
        h.send(Command::Join {
            room: "jogos".to_owned(),
        });
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { .. }));
        assert_eq!(h.backend.sharing(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn capture_that_ends_by_itself_reports_why_and_unpublishes() {
        let mut h = harness();
        h.join("geral").await;
        start_share(&h, "s1");
        assert_eq!(h.next().await, Event::ScreenShareStarted);
        h.backend.emit(BackendEvent::ScreenShareEnded {
            reason: CaptureEnd::TargetClosed,
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::CaptureEnded(CaptureEnd::TargetClosed))
        );
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.backend.sharing(), None);
        h.backend.emit(BackendEvent::ScreenShareEnded {
            reason: CaptureEnd::TargetClosed,
        });
        h.send(Command::RefreshCaptureTargets);
        assert!(matches!(h.next().await, Event::CaptureTargetsUpdated(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn noise_gate_command_is_clamped_and_mic_level_is_forwarded() {
        let mut h = harness();
        h.join("geral").await;
        h.send(Command::SetNoiseGate(Some(-90)));
        h.send(Command::SetNoiseGate(Some(-5)));
        h.send(Command::SetNoiseGate(Some(-45)));
        h.send(Command::SetNoiseGate(None));
        h.send(Command::RefreshAudioDevices);
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        let gate_calls: Vec<_> = h
            .backend
            .calls()
            .into_iter()
            .filter(|c| matches!(c, BackendCall::SetNoiseGate(_)))
            .collect();
        assert_eq!(
            gate_calls,
            [
                BackendCall::SetNoiseGate(Some(-70)),
                BackendCall::SetNoiseGate(Some(-20)),
                BackendCall::SetNoiseGate(Some(-45)),
                BackendCall::SetNoiseGate(None),
            ]
        );

        h.backend.emit(BackendEvent::MicLevel {
            level_db: -33,
            gate_open: true,
        });
        assert_eq!(
            h.next().await,
            Event::MicLevel {
                level_db: -33,
                gate_open: true
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn remote_share_events_are_forwarded() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.emit(BackendEvent::RemoteScreenShareStarted {
            identity: "ana".to_owned(),
        });
        h.backend.emit(BackendEvent::RemoteScreenShareStopped {
            identity: "ana".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::RemoteScreenShareStarted {
                identity: "ana".to_owned()
            }
        );
        assert_eq!(
            h.next().await,
            Event::RemoteScreenShareStopped {
                identity: "ana".to_owned()
            }
        );
    }

    fn frame(value: u8) -> VideoFrame {
        VideoFrame {
            width: 1,
            height: 1,
            rgba: vec![value; 4],
        }
    }

    async fn watch(h: &mut Harness, identity: &str) -> crate::video::FrameSlot {
        h.send(Command::WatchScreenShare {
            identity: identity.to_owned(),
        });
        let Event::WatchingStarted {
            identity: got,
            frames,
        } = h.next().await
        else {
            panic!("expected WatchingStarted");
        };
        assert_eq!(got, identity);
        frames
    }

    #[tokio::test(start_paused = true)]
    async fn watch_switch_and_stop_reach_the_backend_and_frames_arrive() {
        let mut h = harness();
        h.send(Command::WatchScreenShare {
            identity: "ana".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::NotInRoom)
        );
        h.join("geral").await;
        let slot_a = watch(&mut h, "ana").await;
        assert_eq!(h.backend.watching().as_deref(), Some("ana"));
        assert!(h.backend.push_frame(frame(1)));
        let mut seen = 0;
        assert_eq!(slot_a.take_if_newer(&mut seen).unwrap().rgba[0], 1);

        h.send(Command::WatchScreenShare {
            identity: "ana".to_owned(),
        });
        let slot_b = watch(&mut h, "bruno").await;
        assert_ne!(slot_a, slot_b);
        assert_eq!(h.backend.watching().as_deref(), Some("bruno"));
        assert!(h.backend.push_frame(frame(2)));
        assert!(slot_a.take_if_newer(&mut seen).is_none());
        let mut seen_b = 0;
        assert_eq!(slot_b.take_if_newer(&mut seen_b).unwrap().rgba[0], 2);

        h.send(Command::StopWatching);
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(h.backend.watching(), None);
        assert!(!h.backend.push_frame(frame(3)));
        let watches = h
            .backend
            .calls()
            .into_iter()
            .filter(|c| matches!(c, BackendCall::WatchScreenShare { .. }))
            .count();
        assert_eq!(watches, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn watching_ends_when_the_share_stops_the_watcher_leaves_or_the_room_is_left() {
        let mut h = harness();
        h.join("geral").await;
        watch(&mut h, "ana").await;
        h.backend.emit(BackendEvent::RemoteScreenShareStopped {
            identity: "ana".to_owned(),
        });
        assert!(matches!(
            h.next().await,
            Event::RemoteScreenShareStopped { .. }
        ));
        assert_eq!(h.next().await, Event::WatchingStopped);

        watch(&mut h, "ana").await;
        h.backend.emit(BackendEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(matches!(h.next().await, Event::ParticipantLeft { .. }));
        assert_eq!(h.next().await, Event::WatchingStopped);

        watch(&mut h, "bruno").await;
        h.backend.emit(BackendEvent::WatchEnded {
            identity: "bruno".to_owned(),
            reason: Some("boom".to_owned()),
        });
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::Video("boom".to_owned()))
        );

        watch(&mut h, "bruno").await;
        h.send(Command::Leave);
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(h.next().await, Event::Left);
    }

    #[tokio::test(start_paused = true)]
    async fn unexpected_disconnect_stops_share_and_watch() {
        let mut h = harness();
        h.join("geral").await;
        start_share(&h, "s1");
        assert_eq!(h.next().await, Event::ScreenShareStarted);
        watch(&mut h, "ana").await;
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
    }

    #[test]
    fn audio_only_session_serves_device_commands_without_tokens() {
        use crate::SessionHandle;

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
        use crate::SessionHandle;

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
        use crate::SessionHandle;

        let handle =
            SessionHandle::spawn(config(), FakeBackend::new(), FakeTokenClient::new()).unwrap();
        handle.send(Command::Leave);
        drop(handle);
    }

    #[test]
    fn config_debug_hides_invite_code() {
        assert!(!format!("{:?}", config()).contains("secret"));
    }

    #[tokio::test(start_paused = true)]
    async fn play_cue_reaches_backend_inside_and_outside_a_room() {
        let mut h = harness();
        h.send(Command::PlayCue(Cue::Muted));
        h.send(Command::PlayCue(Cue::Unmuted));
        h.join("geral").await;
        h.send(Command::PlayCue(Cue::Deafened));
        h.send(Command::PlayCue(Cue::Undeafened));
        h.send(Command::RefreshAudioDevices);
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        let cues: Vec<_> = h
            .backend
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                BackendCall::PlayCue(cue) => Some(cue),
                _ => None,
            })
            .collect();
        assert_eq!(
            cues,
            vec![Cue::Muted, Cue::Unmuted, Cue::Deafened, Cue::Undeafened]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn participant_volume_clamps_works_outside_room_and_survives_room_switch_and_reconnect() {
        let mut h = harness();
        h.send(Command::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 250,
        });
        h.send(Command::SetParticipantVolume {
            identity: "bob".to_owned(),
            percent: 60,
        });
        h.join("geral").await;
        assert_eq!(h.backend.participant_volume_gain("alice"), Some(2.0));
        assert_eq!(h.backend.participant_volume_gain("bob"), Some(0.6));

        h.send(Command::SetParticipantVolume {
            identity: "bob".to_owned(),
            percent: 100,
        });
        h.send(Command::SetParticipantVolume {
            identity: "carol".to_owned(),
            percent: 0,
        });
        h.send(Command::RefreshAudioDevices);
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        assert_eq!(h.backend.participant_volume_gain("bob"), None);
        assert_eq!(h.backend.participant_volume_gain("carol"), Some(0.0));

        // Simulate disconnect + automatic reconnect: 100% ("bob") stays omitted, others reapply.
        let calls_before_reconnect = h.backend.calls().len();
        assert!(h.backend.emit(BackendEvent::Disconnected {
            reason: "dropped".to_owned(),
        }));
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert_eq!(h.next().await, Event::ParticipantList(Vec::new()));
        let reconnect_calls = &h.backend.calls()[calls_before_reconnect..];
        assert_eq!(
            reconnect_calls,
            [
                BackendCall::Connect {
                    url: "ws://fake.invalid".to_owned(),
                    token: "token-geral-2".to_owned(),
                },
                BackendCall::SetMicrophoneMuted(false),
                BackendCall::SetOutputMuted(false),
                BackendCall::SetParticipantVolume {
                    identity: "alice".to_owned(),
                    percent: 200,
                },
                BackendCall::SetParticipantVolume {
                    identity: "carol".to_owned(),
                    percent: 0,
                },
            ]
        );

        // Switch room: active entries survive room switch too.
        let calls_before_switch = h.backend.calls().len();
        h.send(Command::Join {
            room: "jogos".to_owned(),
        });
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { room, .. } if room == "jogos"));
        let switch_calls = &h.backend.calls()[calls_before_switch..];
        assert_eq!(
            switch_calls,
            [
                BackendCall::Disconnect,
                BackendCall::Connect {
                    url: "ws://fake.invalid".to_owned(),
                    token: "token-jogos-3".to_owned(),
                },
                BackendCall::SetMicrophoneMuted(false),
                BackendCall::SetOutputMuted(false),
                BackendCall::SetParticipantVolume {
                    identity: "alice".to_owned(),
                    percent: 200,
                },
                BackendCall::SetParticipantVolume {
                    identity: "carol".to_owned(),
                    percent: 0,
                },
            ]
        );
    }
}
