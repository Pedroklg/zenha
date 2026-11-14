use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Arc;

#[cfg(windows)]
use rooms_core::LiveKitBackend;
#[cfg(not(windows))]
use rooms_core::NoMediaBackend;
use rooms_core::SessionHandle;
use rooms_core::{
    AudioDevice, CaptureEnd, CaptureKind, CaptureTarget, Command, Event, FrameSink, FrameSlot,
    FrameSource, NoiseSuppression, Participant, SessionConfig, SessionError, SharePreset, SlotSink,
    TestPatternSource, TransferState, VideoFrame, is_image_mime,
};

use crate::settings::NoiseMode;
use crate::ui_model::{
    AudioDeviceView, CaptureTargetView, ChatMessageView, Credentials, FeedHandle, FileTransferView,
    FrameFeed, ParticipantView, SessionPort, SharePresetView, TargetKindView, TransferStateView,
    UiCommand, UiEvent, UiFrame,
};

pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct CoreSession {
    handle: Option<SessionHandle>,
    translator: Translator,
    queued: VecDeque<UiEvent>,
    wake: Wake,
}

#[cfg(windows)]
fn media_backend(_display_name: &str) -> LiveKitBackend {
    LiveKitBackend::new()
}

#[cfg(not(windows))]
fn media_backend(display_name: &str) -> NoMediaBackend {
    NoMediaBackend::new(display_name)
}

impl CoreSession {
    /// Starts with a session that only serves audio device commands, so the
    /// settings panel works on the login screen; `configure` replaces it.
    pub fn new(wake: Wake) -> Self {
        let mut session = Self {
            handle: None,
            translator: Translator::default(),
            queued: VecDeque::new(),
            wake,
        };
        match SessionHandle::spawn_audio_only(media_backend("")) {
            Ok(handle) => session.install(handle),
            Err(err) => tracing::warn!(%err, "could not start the audio device session"),
        }
        session
    }

    fn install(&mut self, handle: SessionHandle) {
        let wake = Arc::clone(&self.wake);
        handle.set_wake_callback(Box::new(move || wake()));
        self.handle = Some(handle);
    }
}

impl SessionPort for CoreSession {
    fn configure(&mut self, credentials: Credentials) {
        self.handle = None;
        self.translator = Translator::default();
        self.queued.clear();

        let backend = media_backend(&credentials.client_id);
        let config = SessionConfig {
            token_server_url: credentials.token_server_url,
            client_id: credentials.client_id,
            display_name: credentials.display_name,
            invite_code: credentials.invite_code,
        };
        match SessionHandle::spawn_with_http_tokens(config, backend) {
            Ok(handle) => self.install(handle),
            Err(err) => self.queued.push_back(UiEvent::Failed {
                message: describe(&err),
            }),
        }
    }

    fn send(&self, command: UiCommand) {
        if let Some(handle) = &self.handle {
            if matches!(command, UiCommand::Join { .. }) && self.translator.room.is_some() {
                self.translator.switch_pending.set(true);
            }
            handle.send(to_core(command));
        }
    }

    fn try_recv(&mut self) -> Option<UiEvent> {
        loop {
            if let Some(event) = self.queued.pop_front() {
                return Some(event);
            }
            let event = self.handle.as_mut()?.try_recv()?;
            self.queued.extend(self.translator.translate(event));
        }
    }
}

fn to_core(command: UiCommand) -> Command {
    match command {
        UiCommand::Join { room } => Command::Join { room },
        UiCommand::Leave => Command::Leave,
        UiCommand::SetMuted(muted) => Command::SetMuted(muted),
        UiCommand::SetDeafened(deafened) => Command::SetDeafened(deafened),
        UiCommand::SendChat { text, save } => Command::SendChat { text, save },
        UiCommand::LoadChatHistory { before } => Command::LoadChatHistory { before },
        UiCommand::DeleteChat { id } => Command::DeleteChat { id },
        UiCommand::SendFile { path } => Command::SendFile { path },
        UiCommand::RefreshRooms => Command::RefreshRooms,
        UiCommand::RefreshAudioDevices => Command::RefreshAudioDevices,
        UiCommand::SetInputDevice(id) => Command::SetInputDevice(id),
        UiCommand::SetOutputDevice(id) => Command::SetOutputDevice(id),
        UiCommand::SetStreamVolume(percent) => Command::SetStreamVolume(percent),
        UiCommand::SetNoiseGate(threshold) => Command::SetNoiseGate(threshold),
        UiCommand::SetNoiseSuppression(mode) => Command::SetNoiseSuppression(match mode {
            NoiseMode::Off => NoiseSuppression::Off,
            NoiseMode::Standard => NoiseSuppression::Standard,
            NoiseMode::Enhanced => NoiseSuppression::Enhanced,
            NoiseMode::Maximum => NoiseSuppression::Maximum,
        }),
        UiCommand::RefreshCaptureTargets => Command::RefreshCaptureTargets,
        UiCommand::StartScreenShare { target_id, preset } => Command::StartScreenShare {
            target_id,
            preset: to_core_preset(preset),
        },
        UiCommand::StopScreenShare => Command::StopScreenShare,
        UiCommand::WatchScreenShare { identity } => Command::WatchScreenShare { identity },
        UiCommand::StopWatching => Command::StopWatching,
        UiCommand::PlayCue(cue) => Command::PlayCue(cue),
    }
}

fn to_core_preset(preset: SharePresetView) -> SharePreset {
    match preset {
        SharePresetView::P720F30 => SharePreset::P720F30,
        SharePresetView::P1080F30 => SharePreset::P1080F30,
        SharePresetView::P1080F60 => SharePreset::P1080F60,
    }
}

fn target_view(target: CaptureTarget) -> CaptureTargetView {
    CaptureTargetView {
        id: target.id,
        name: target.name,
        kind: match target.kind {
            CaptureKind::Screen => TargetKindView::Screen,
            CaptureKind::Window => TargetKindView::Window,
        },
    }
}

struct Pixels(Arc<VideoFrame>);

impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.0.rgba
    }
}

struct SlotFeed(FrameSlot);

impl FrameFeed for SlotFeed {
    fn take_newer(&self, last_seen: &mut u64) -> Option<UiFrame> {
        let frame = self.0.take_if_newer(last_seen)?;
        if !frame.is_valid() {
            return None;
        }
        Some(UiFrame {
            width: frame.width as usize,
            height: frame.height as usize,
            rgba: Arc::new(Pixels(frame)),
        })
    }
}

fn feed_handle(slot: FrameSlot) -> FeedHandle {
    FeedHandle(Arc::new(SlotFeed(slot)))
}

/// Moving colour bars written into a frame feed, for the `--demo` mode.
pub struct TestFeed {
    handle: FeedHandle,
    source: TestPatternSource,
}

impl TestFeed {
    pub fn start() -> Option<Self> {
        let slot = FrameSlot::new();
        let mut source = TestPatternSource::new(1280, 720);
        let target = source.list_targets().ok()?.into_iter().next()?;
        let sink: Box<dyn FrameSink> = Box::new(SlotSink(slot.clone()));
        source.start(&target, 30, sink).ok()?;
        Some(Self {
            handle: feed_handle(slot),
            source,
        })
    }

    pub fn handle(&self) -> FeedHandle {
        self.handle.clone()
    }
}

impl Drop for TestFeed {
    fn drop(&mut self) {
        self.source.stop();
    }
}

fn describe(error: &SessionError) -> String {
    match error {
        SessionError::InviteRejected => "Senha recusada pelo servidor.".to_owned(),
        SessionError::TokenServer(reason) => {
            format!("Não foi possível falar com o servidor de tokens: {reason}")
        }
        SessionError::Connection(reason) => {
            format!("Falha na conexão com o servidor de mídia: {reason}")
        }
        SessionError::ShareUnavailable => {
            "A transmissão de tela não está disponível nesta versão.".to_owned()
        }
        SessionError::ShareActive => "Já existe uma transmissão em andamento.".to_owned(),
        SessionError::TargetNotFound(_) => {
            "A tela ou janela escolhida não está mais disponível.".to_owned()
        }
        SessionError::CaptureEnded(reason) => match reason {
            CaptureEnd::TargetClosed => {
                "A transmissão terminou: a tela ou janela foi fechada ou ficou indisponível."
                    .to_owned()
            }
            CaptureEnd::RepeatedErrors => {
                "A transmissão terminou: a captura falhou várias vezes seguidas.".to_owned()
            }
            CaptureEnd::Failed(detail) => {
                format!("A transmissão terminou: a captura falhou ({detail}).")
            }
        },
        SessionError::Video(detail) => format!("Falha no vídeo: {detail}"),
        SessionError::RateLimited => {
            "Muitas mensagens seguidas. Espere alguns segundos.".to_owned()
        }
        SessionError::Unauthorized => {
            "Não foi possível salvar: sessão expirada. Tente de novo.".to_owned()
        }
        SessionError::Chat(_) => {
            "O servidor não respondeu ao chat salvo. A mensagem não foi salva; tente de novo."
                .to_owned()
        }
        other => other.to_string(),
    }
}

fn is_share_error(error: &SessionError) -> bool {
    matches!(
        error,
        SessionError::ShareUnavailable
            | SessionError::ShareActive
            | SessionError::TargetNotFound(_)
            | SessionError::CaptureEnded(_)
            | SessionError::Video(_)
    )
}

fn device_view(device: AudioDevice) -> AudioDeviceView {
    AudioDeviceView {
        id: device.id,
        name: device.name,
    }
}

fn view(participant: Participant) -> ParticipantView {
    ParticipantView {
        identity: participant.identity,
        name: participant.name,
        muted: participant.muted,
        speaking: participant.speaking,
    }
}

/// Converte eventos do núcleo em eventos de interface, guardando o que a
/// interface espera receber inteiro (participante completo, sala atual).
#[derive(Default)]
struct Translator {
    room: Option<String>,
    local_identity: String,
    participants: Vec<ParticipantView>,
    switch_pending: Cell<bool>,
}

impl Translator {
    fn chat_message_view(&self, message: rooms_protocol::ChatMessage) -> ChatMessageView {
        let sender = self
            .participants
            .iter()
            .find(|p| p.identity == message.sender)
            .map_or_else(|| message.sender.clone(), |p| p.name.clone());
        ChatMessageView {
            id: message.id,
            sender_id: message.sender,
            sender,
            sent_at_unix_ms: message.sent_at_unix_ms,
            text: message.text,
            saved: message.saved,
        }
    }

    fn translate(&mut self, event: Event) -> Vec<UiEvent> {
        match event {
            Event::Connecting => vec![UiEvent::Connecting],
            Event::Joined {
                room,
                local_identity,
                participants,
            } => {
                self.room = Some(room.clone());
                self.local_identity = local_identity.clone();
                self.participants = participants.into_iter().map(view).collect();
                vec![UiEvent::Joined {
                    room,
                    local_identity,
                    participants: self.participants.clone(),
                }]
            }
            Event::ParticipantList(participants) => {
                self.participants = participants.into_iter().map(view).collect();
                match &self.room {
                    Some(room) => vec![UiEvent::Joined {
                        room: room.clone(),
                        local_identity: self.local_identity.clone(),
                        participants: self.participants.clone(),
                    }],
                    None => Vec::new(),
                }
            }
            Event::Reconnecting => vec![UiEvent::Reconnecting],
            Event::Reconnected => vec![UiEvent::Reconnected],
            Event::Left => {
                self.participants.clear();
                // On a room switch the core reports Left before the new Joined;
                // the interface only wants Left when the user really left.
                if self.switch_pending.take() {
                    Vec::new()
                } else {
                    self.room = None;
                    self.local_identity.clear();
                    vec![UiEvent::Left]
                }
            }
            Event::Failed(error) => {
                self.switch_pending.set(false);
                self.room = None;
                self.local_identity.clear();
                self.participants.clear();
                if matches!(error, SessionError::InviteRejected) {
                    vec![UiEvent::InviteRejected]
                } else {
                    vec![UiEvent::Failed {
                        message: describe(&error),
                    }]
                }
            }
            Event::ParticipantJoined(participant) => {
                let participant = view(participant);
                self.participants
                    .retain(|p| p.identity != participant.identity);
                self.participants.push(participant.clone());
                vec![UiEvent::ParticipantJoined(participant)]
            }
            Event::ParticipantLeft { identity } => {
                self.participants.retain(|p| p.identity != identity);
                vec![UiEvent::ParticipantLeft { identity }]
            }
            Event::ParticipantMuteChanged { identity, muted } => {
                self.update(&identity, |p| p.muted = muted)
            }
            Event::ParticipantSpeakingChanged { identity, speaking } => {
                self.update(&identity, |p| p.speaking = speaking)
            }
            Event::ChatReceived(message) => {
                vec![UiEvent::ChatReceived(self.chat_message_view(message))]
            }
            Event::FileTransfer(transfer) => {
                let sender = self
                    .participants
                    .iter()
                    .find(|p| p.identity == transfer.sender)
                    .map_or(transfer.sender, |p| p.name.clone());
                vec![UiEvent::FileTransfer(FileTransferView {
                    id: transfer.id,
                    sender,
                    is_image: is_image_mime(&transfer.mime),
                    name: transfer.name,
                    size: transfer.size,
                    outgoing: transfer.outgoing,
                    sent_at_unix_ms: transfer.sent_at_unix_ms,
                    state: match transfer.state {
                        TransferState::InProgress { bytes } => {
                            TransferStateView::InProgress { bytes }
                        }
                        TransferState::Done { path } => TransferStateView::Done { path },
                        TransferState::Failed(message) => TransferStateView::Failed(message),
                    },
                })]
            }
            Event::RoomsUpdated(rooms) => vec![UiEvent::RoomsUpdated(rooms)],
            Event::AudioDevicesUpdated(devices) => vec![UiEvent::AudioDevicesUpdated {
                inputs: devices.inputs.into_iter().map(device_view).collect(),
                outputs: devices.outputs.into_iter().map(device_view).collect(),
                selected_input: devices.selected_input,
                selected_output: devices.selected_output,
            }],
            Event::CaptureTargetsUpdated(targets) => vec![UiEvent::CaptureTargetsUpdated(
                targets.into_iter().map(target_view).collect(),
            )],
            Event::MicLevel {
                level_db,
                gate_open,
            } => vec![UiEvent::MicLevel {
                level_db,
                gate_open,
            }],
            Event::ScreenShareStarted => vec![UiEvent::ScreenShareStarted],
            Event::ScreenShareStopped => vec![UiEvent::ScreenShareStopped],
            Event::RemoteScreenShareStarted { identity } => {
                vec![UiEvent::RemoteScreenShareStarted { identity }]
            }
            Event::RemoteScreenShareStopped { identity } => {
                vec![UiEvent::RemoteScreenShareStopped { identity }]
            }
            Event::WatchingStarted { identity, frames } => vec![UiEvent::WatchingStarted {
                identity,
                feed: feed_handle(frames),
            }],
            Event::WatchingStopped => vec![UiEvent::WatchingStopped],
            Event::ChatHistory {
                messages,
                has_more,
                before,
            } => {
                let messages = messages
                    .into_iter()
                    .map(|item| ChatMessageView {
                        id: item.message.id,
                        sender_id: item.sender_id,
                        sender: item.message.sender,
                        sent_at_unix_ms: item.message.sent_at_unix_ms,
                        text: item.message.text,
                        saved: item.message.saved,
                    })
                    .collect();
                vec![UiEvent::ChatHistory {
                    messages,
                    has_more,
                    before,
                }]
            }
            Event::ChatDeleted { id, sender } => vec![UiEvent::ChatDeleted { id, sender }],
            Event::CommandFailed(error) => {
                tracing::warn!(%error, "command failed");
                match &error {
                    SessionError::InviteRejected => vec![UiEvent::InviteRejected],
                    SessionError::RateLimited
                    | SessionError::Unauthorized
                    | SessionError::Chat(_) => vec![UiEvent::ChatFailed {
                        message: describe(&error),
                    }],
                    SessionError::TokenServer(_) => vec![UiEvent::RoomsFailed {
                        message: describe(&error),
                    }],
                    _ if is_share_error(&error) => vec![UiEvent::ShareFailed {
                        message: describe(&error),
                    }],
                    _ => Vec::new(),
                }
            }
        }
    }

    fn update(
        &mut self,
        identity: &str,
        change: impl FnOnce(&mut ParticipantView),
    ) -> Vec<UiEvent> {
        match self
            .participants
            .iter_mut()
            .find(|p| p.identity == identity)
        {
            Some(participant) => {
                change(participant);
                vec![UiEvent::ParticipantUpdated(participant.clone())]
            }
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use rooms_protocol::{ChatMessage, SavedChatMessage};

    use super::*;

    fn participant(identity: &str, name: &str) -> Participant {
        Participant {
            identity: identity.to_owned(),
            name: name.to_owned(),
            muted: false,
            speaking: false,
        }
    }

    fn joined(translator: &mut Translator) {
        translator.translate(Event::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![participant("pedro", "Pedro"), participant("ana", "Ana")],
        });
    }

    #[test]
    fn mute_and_speaking_changes_become_full_participant_updates() {
        let mut translator = Translator::default();
        joined(&mut translator);
        let events = translator.translate(Event::ParticipantMuteChanged {
            identity: "ana".to_owned(),
            muted: true,
        });
        assert_eq!(
            events,
            vec![UiEvent::ParticipantUpdated(ParticipantView {
                identity: "ana".to_owned(),
                name: "Ana".to_owned(),
                muted: true,
                speaking: false,
            })]
        );
        let events = translator.translate(Event::ParticipantSpeakingChanged {
            identity: "ana".to_owned(),
            speaking: true,
        });
        assert!(matches!(&events[..], [UiEvent::ParticipantUpdated(p)] if p.muted && p.speaking));
    }

    #[test]
    fn change_for_unknown_participant_is_dropped() {
        let mut translator = Translator::default();
        joined(&mut translator);
        let events = translator.translate(Event::ParticipantMuteChanged {
            identity: "x".to_owned(),
            muted: true,
        });
        assert!(events.is_empty());
    }

    #[test]
    fn participant_list_is_reported_as_a_join_of_the_current_room() {
        let mut translator = Translator::default();
        joined(&mut translator);
        let events =
            translator.translate(Event::ParticipantList(vec![participant("pedro", "Pedro")]));
        assert!(matches!(
            &events[..],
            [UiEvent::Joined { room, local_identity, participants }]
                if room == "geral" && local_identity == "pedro" && participants.len() == 1
        ));
    }

    #[test]
    fn chat_sender_identity_is_replaced_by_the_display_name() {
        let mut translator = Translator::default();
        joined(&mut translator);
        let events = translator.translate(Event::ChatReceived(ChatMessage {
            id: "1".to_owned(),
            sender: "ana".to_owned(),
            sent_at_unix_ms: 0,
            text: "oi".to_owned(),
            saved: false,
        }));
        assert!(matches!(
            &events[..],
            [UiEvent::ChatReceived(m)] if m.sender_id == "ana" && m.sender == "Ana"
        ));
    }

    #[test]
    fn file_transfer_gets_display_name_and_image_flag() {
        let mut translator = Translator::default();
        joined(&mut translator);
        let transfer = |mime: &str| rooms_core::FileTransfer {
            id: "t1".to_owned(),
            sender: "ana".to_owned(),
            name: "foto.png".to_owned(),
            size: 10,
            mime: mime.to_owned(),
            outgoing: false,
            sent_at_unix_ms: 5,
            state: TransferState::InProgress { bytes: 4 },
        };
        let events = translator.translate(Event::FileTransfer(transfer("image/png")));
        assert!(matches!(
            &events[..],
            [UiEvent::FileTransfer(v)]
                if v.sender == "Ana"
                    && v.is_image
                    && v.state == TransferStateView::InProgress { bytes: 4 }
        ));
        let events = translator.translate(Event::FileTransfer(transfer("application/pdf")));
        assert!(matches!(&events[..], [UiEvent::FileTransfer(v)] if !v.is_image));
    }

    #[test]
    fn send_file_command_is_forwarded() {
        let command = to_core(UiCommand::SendFile {
            path: "a.png".into(),
        });
        assert_eq!(
            command,
            Command::SendFile {
                path: "a.png".into()
            }
        );
    }

    #[test]
    fn left_is_hidden_during_a_room_switch_and_shown_otherwise() {
        let mut translator = Translator::default();
        joined(&mut translator);
        translator.switch_pending.set(true);
        assert!(translator.translate(Event::Left).is_empty());
        assert_eq!(translator.translate(Event::Left), vec![UiEvent::Left]);
        assert!(translator.room.is_none());
    }

    #[test]
    fn errors_are_described_in_portuguese() {
        let mut translator = Translator::default();
        assert_eq!(
            describe(&SessionError::InviteRejected),
            "Senha recusada pelo servidor."
        );
        let events = translator.translate(Event::Failed(SessionError::TokenServer("500".into())));
        assert_eq!(
            events,
            vec![UiEvent::Failed {
                message: "Não foi possível falar com o servidor de tokens: 500".to_owned()
            }]
        );
    }

    #[test]
    fn invite_rejection_and_room_refresh_errors_translate_to_ui_events() {
        let mut translator = Translator::default();
        assert_eq!(
            translator.translate(Event::Failed(SessionError::InviteRejected)),
            vec![UiEvent::InviteRejected]
        );
        assert_eq!(
            translator.translate(Event::CommandFailed(SessionError::InviteRejected)),
            vec![UiEvent::InviteRejected]
        );
        assert_eq!(
            translator.translate(Event::CommandFailed(SessionError::TokenServer(
                "offline".into()
            ))),
            vec![UiEvent::RoomsFailed {
                message: "Não foi possível falar com o servidor de tokens: offline".to_owned(),
            }]
        );
    }

    #[test]
    fn commands_map_one_to_one() {
        assert_eq!(
            to_core(UiCommand::Join {
                room: "geral".to_owned()
            }),
            Command::Join {
                room: "geral".to_owned()
            }
        );
        assert_eq!(to_core(UiCommand::RefreshRooms), Command::RefreshRooms);
        assert_eq!(
            to_core(UiCommand::RefreshAudioDevices),
            Command::RefreshAudioDevices
        );
        assert_eq!(
            to_core(UiCommand::SetInputDevice(Some("m1".to_owned()))),
            Command::SetInputDevice(Some("m1".to_owned()))
        );
        assert_eq!(
            to_core(UiCommand::SetOutputDevice(None)),
            Command::SetOutputDevice(None)
        );
        assert_eq!(
            to_core(UiCommand::PlayCue(rooms_core::Cue::Muted)),
            Command::PlayCue(rooms_core::Cue::Muted)
        );
    }

    #[test]
    fn share_commands_and_presets_map_one_to_one() {
        assert_eq!(
            to_core(UiCommand::StartScreenShare {
                target_id: "screen:1".to_owned(),
                preset: SharePresetView::P1080F60,
            }),
            Command::StartScreenShare {
                target_id: "screen:1".to_owned(),
                preset: SharePreset::P1080F60,
            }
        );
        assert_eq!(
            to_core(UiCommand::StopScreenShare),
            Command::StopScreenShare
        );
        assert_eq!(
            to_core(UiCommand::WatchScreenShare {
                identity: "ana".to_owned()
            }),
            Command::WatchScreenShare {
                identity: "ana".to_owned()
            }
        );
        assert_eq!(to_core(UiCommand::StopWatching), Command::StopWatching);
        assert_eq!(
            to_core(UiCommand::RefreshCaptureTargets),
            Command::RefreshCaptureTargets
        );
    }

    #[test]
    fn share_events_are_forwarded_and_share_errors_are_shown_in_portuguese() {
        let mut translator = Translator::default();
        let events = translator.translate(Event::CaptureTargetsUpdated(vec![CaptureTarget {
            id: "window:7".to_owned(),
            name: "Bloco".to_owned(),
            kind: CaptureKind::Window,
        }]));
        assert_eq!(
            events,
            vec![UiEvent::CaptureTargetsUpdated(vec![CaptureTargetView {
                id: "window:7".to_owned(),
                name: "Bloco".to_owned(),
                kind: TargetKindView::Window,
            }])]
        );
        assert_eq!(
            translator.translate(Event::RemoteScreenShareStarted {
                identity: "ana".to_owned()
            }),
            vec![UiEvent::RemoteScreenShareStarted {
                identity: "ana".to_owned()
            }]
        );
        let events = translator.translate(Event::CommandFailed(SessionError::CaptureEnded(
            CaptureEnd::TargetClosed,
        )));
        assert!(
            matches!(&events[..], [UiEvent::ShareFailed { message }] if message.contains("fechada"))
        );
        assert_eq!(
            translator.translate(Event::MicLevel {
                level_db: -42,
                gate_open: true
            }),
            vec![UiEvent::MicLevel {
                level_db: -42,
                gate_open: true
            }]
        );
        assert_eq!(
            to_core(UiCommand::SetNoiseGate(Some(-45))),
            Command::SetNoiseGate(Some(-45))
        );
        assert!(
            translator
                .translate(Event::CommandFailed(SessionError::NotInRoom))
                .is_empty()
        );
    }

    #[test]
    fn watching_started_carries_a_feed_that_yields_each_frame_once() {
        let mut translator = Translator::default();
        let slot = FrameSlot::new();
        let events = translator.translate(Event::WatchingStarted {
            identity: "ana".to_owned(),
            frames: slot.clone(),
        });
        let [UiEvent::WatchingStarted { identity, feed }] = &events[..] else {
            panic!("unexpected {events:?}");
        };
        assert_eq!(identity, "ana");
        let mut seen = 0;
        assert!(feed.take_newer(&mut seen).is_none());
        slot.push(VideoFrame {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        });
        let frame = feed.take_newer(&mut seen).unwrap();
        assert_eq!((frame.width, frame.height), (1, 1));
        assert_eq!(frame.bytes(), &[1, 2, 3, 255]);
        assert!(feed.take_newer(&mut seen).is_none());
    }

    #[test]
    fn test_feed_produces_frames() {
        let feed = TestFeed::start().unwrap();
        let handle = feed.handle();
        let mut seen = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut frame = None;
        while frame.is_none() && std::time::Instant::now() < deadline {
            frame = handle.take_newer(&mut seen);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let frame = frame.unwrap();
        assert_eq!((frame.width, frame.height), (1280, 720));
        assert_eq!(frame.bytes().len(), 1280 * 720 * 4);
    }

    #[test]
    fn audio_devices_become_plain_ui_structs() {
        let mut translator = Translator::default();
        let events = translator.translate(Event::AudioDevicesUpdated(rooms_core::AudioDevices {
            inputs: vec![AudioDevice {
                id: "m1".to_owned(),
                name: "Mic".to_owned(),
            }],
            outputs: Vec::new(),
            selected_input: Some("m1".to_owned()),
            selected_output: None,
        }));
        assert_eq!(
            events,
            vec![UiEvent::AudioDevicesUpdated {
                inputs: vec![AudioDeviceView {
                    id: "m1".to_owned(),
                    name: "Mic".to_owned()
                }],
                outputs: Vec::new(),
                selected_input: Some("m1".to_owned()),
                selected_output: None,
            }]
        );
    }

    #[test]
    fn chat_commands_map_with_save_history_and_delete() {
        assert_eq!(
            to_core(UiCommand::SendChat {
                text: "oi".to_owned(),
                save: true,
            }),
            Command::SendChat {
                text: "oi".to_owned(),
                save: true,
            }
        );
        assert_eq!(
            to_core(UiCommand::LoadChatHistory {
                before: Some("m1".to_owned()),
            }),
            Command::LoadChatHistory {
                before: Some("m1".to_owned()),
            }
        );
        assert_eq!(
            to_core(UiCommand::DeleteChat {
                id: "m2".to_owned(),
            }),
            Command::DeleteChat {
                id: "m2".to_owned(),
            }
        );
    }

    #[test]
    fn chat_history_and_delete_events_keep_sender_identity_and_name() {
        let mut translator = Translator::default();
        joined(&mut translator);
        let events = translator.translate(Event::ChatHistory {
            messages: vec![SavedChatMessage {
                message: ChatMessage {
                    id: "m1".to_owned(),
                    sender: "Ana Silva".to_owned(),
                    sent_at_unix_ms: 1000,
                    text: "salva".to_owned(),
                    saved: true,
                },
                sender_id: "ana".to_owned(),
            }],
            has_more: true,
            before: Some("m2".to_owned()),
        });
        assert_eq!(
            events,
            vec![UiEvent::ChatHistory {
                messages: vec![ChatMessageView {
                    id: "m1".to_owned(),
                    sender_id: "ana".to_owned(),
                    sender: "Ana Silva".to_owned(),
                    sent_at_unix_ms: 1000,
                    text: "salva".to_owned(),
                    saved: true,
                }],
                has_more: true,
                before: Some("m2".to_owned()),
            }]
        );

        let events = translator.translate(Event::ChatDeleted {
            id: "m1".to_owned(),
            sender: "ana".to_owned(),
        });
        assert_eq!(
            events,
            vec![UiEvent::ChatDeleted {
                id: "m1".to_owned(),
                sender: "ana".to_owned(),
            }]
        );
    }

    #[test]
    fn chat_command_failures_translate_to_portuguese_notices() {
        let mut translator = Translator::default();
        joined(&mut translator);
        assert_eq!(
            translator.translate(Event::CommandFailed(SessionError::RateLimited)),
            vec![UiEvent::ChatFailed {
                message: "Muitas mensagens seguidas. Espere alguns segundos.".to_owned(),
            }]
        );
        assert_eq!(
            translator.translate(Event::CommandFailed(SessionError::Unauthorized)),
            vec![UiEvent::ChatFailed {
                message: "Não foi possível salvar: sessão expirada. Tente de novo.".to_owned(),
            }]
        );
        assert!(matches!(
            &translator.translate(Event::CommandFailed(SessionError::Chat("503".into())))[..],
            [UiEvent::ChatFailed { .. }]
        ));
        assert_eq!(
            translator.translate(Event::CommandFailed(SessionError::TokenServer(
                "offline".into()
            ))),
            vec![UiEvent::RoomsFailed {
                message: "Não foi possível falar com o servidor de tokens: offline".to_owned(),
            }]
        );
    }
}
