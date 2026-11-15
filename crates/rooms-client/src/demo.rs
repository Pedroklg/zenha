use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rooms_protocol::RoomSummary;

use crate::core_session::TestFeed;
use crate::ui_model::{
    AudioDeviceView, CaptureTargetView, ChatMessageView, FileTransferView, ParticipantView,
    SessionPort, TargetKindView, TransferStateView, UiCommand, UiEvent,
};

fn is_image_name(name: &str) -> bool {
    rooms_core::is_image_mime(rooms_core::mime_for_name(name))
}

pub const SELF_IDENTITY: &str = "demo-self";
pub const SELF_NAME: &str = "Você";
pub const DEMO_SERVER_URL: &str = "https://demo.zenha.local";
pub const DEMO_PASSWORD: &str = "demo";
const FAILING_ROOM: &str = "falha";
const REPLIES: [&str; 4] = [
    "Boa!",
    "Haha, verdade.",
    "Já estou chegando.",
    "Tá ouvindo bem?",
];

#[derive(Debug, Clone, Copy)]
pub struct DemoDelays {
    pub join: Duration,
    pub late_participant: Duration,
    pub reply: Duration,
    pub remote_share: Duration,
}

impl Default for DemoDelays {
    fn default() -> Self {
        Self {
            join: Duration::from_millis(900),
            late_participant: Duration::from_secs(4),
            reply: Duration::from_millis(1200),
            remote_share: Duration::from_secs(2),
        }
    }
}

impl DemoDelays {
    #[cfg(test)]
    pub const fn instant() -> Self {
        Self {
            join: Duration::ZERO,
            late_participant: Duration::ZERO,
            reply: Duration::ZERO,
            remote_share: Duration::ZERO,
        }
    }
}

struct Scheduled {
    due: Instant,
    seq: u64,
    event: UiEvent,
}

pub struct DemoSession {
    commands_tx: Sender<UiCommand>,
    commands_rx: Receiver<UiCommand>,
    delays: DemoDelays,
    scheduled: Vec<Scheduled>,
    next_seq: u64,
    room: Option<String>,
    participants: Vec<ParticipantView>,
    muted: bool,
    chat_counter: u64,
    file_counter: u64,
    input: Option<String>,
    output: Option<String>,
    share_preview: Option<TestFeed>,
    ana_sharing: bool,
    watch: Option<TestFeed>,
}

fn demo_targets() -> Vec<CaptureTargetView> {
    let target = |id: &str, name: &str, kind| CaptureTargetView {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
    };
    vec![
        target(
            "demo:screen:0",
            "Tela 1 (principal)",
            TargetKindView::Screen,
        ),
        target("demo:screen:1", "Tela 2", TargetKindView::Screen),
        target(
            "demo:window:0",
            "Navegador - Demonstração",
            TargetKindView::Window,
        ),
        target("demo:window:1", "Editor de texto", TargetKindView::Window),
    ]
}

fn demo_devices(kind: &str, names: &[&str]) -> Vec<AudioDeviceView> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| AudioDeviceView {
            id: format!("demo-{kind}-{index}"),
            name: (*name).to_owned(),
        })
        .collect()
}

impl DemoSession {
    pub fn new() -> Self {
        Self::with_delays(DemoDelays::default())
    }

    pub fn with_delays(delays: DemoDelays) -> Self {
        let (commands_tx, commands_rx) = channel();
        Self {
            commands_tx,
            commands_rx,
            delays,
            scheduled: Vec::new(),
            next_seq: 0,
            room: None,
            participants: Vec::new(),
            muted: false,
            chat_counter: 0,
            file_counter: 0,
            input: None,
            output: None,
            share_preview: None,
            ana_sharing: false,
            watch: None,
        }
    }

    fn reset_share(&mut self) {
        self.share_preview = None;
        self.ana_sharing = false;
        self.watch = None;
    }

    fn audio_devices_event(&self) -> UiEvent {
        UiEvent::AudioDevicesUpdated {
            inputs: demo_devices("in", &["Microfone (headset USB)", "Microfone embutido"]),
            outputs: demo_devices(
                "out",
                &["Alto-falantes (Realtek)", "Fones de ouvido (headset USB)"],
            ),
            selected_input: self.input.clone(),
            selected_output: self.output.clone(),
        }
    }

    fn schedule(&mut self, after: Duration, event: UiEvent) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.scheduled.push(Scheduled {
            due: Instant::now() + after,
            seq,
            event,
        });
    }

    fn participant(identity: &str, name: &str) -> ParticipantView {
        ParticipantView {
            identity: identity.to_owned(),
            name: name.to_owned(),
            muted: false,
            speaking: false,
        }
    }

    fn handle(&mut self, command: UiCommand) {
        match command {
            UiCommand::Join { room } => self.join(room),
            UiCommand::Leave => {
                self.scheduled.clear();
                self.room = None;
                self.participants.clear();
                self.reset_share();
                self.schedule(Duration::ZERO, UiEvent::Left);
            }
            UiCommand::SetMuted(muted) => self.set_muted(muted),
            UiCommand::SetDeafened(_) => {}
            UiCommand::SendChat { text, save } => self.chat(text, save),
            UiCommand::LoadChatHistory { before } => self.load_chat_history(before),
            UiCommand::DeleteChat { id } => self.delete_chat(id),
            UiCommand::SendFile { path } => self.send_file(path),
            UiCommand::RefreshAudioDevices => self.publish_audio_devices(),
            UiCommand::SetInputDevice(id) => {
                self.input = id;
                self.publish_audio_devices();
            }
            UiCommand::SetNoiseSuppression(_)
            | UiCommand::SetStreamVolume(_)
            | UiCommand::SetParticipantVolume { .. }
            | UiCommand::SetNoiseGate(_)
            | UiCommand::PlayCue(_) => {}
            UiCommand::SetOutputDevice(id) => {
                self.output = id;
                self.publish_audio_devices();
            }
            UiCommand::RefreshCaptureTargets => {
                self.scheduled
                    .retain(|s| !matches!(s.event, UiEvent::CaptureThumbnail { .. }));
                let targets = demo_targets();
                self.schedule(
                    Duration::ZERO,
                    UiEvent::CaptureTargetsUpdated(targets.clone()),
                );
                for (index, target) in targets.into_iter().enumerate() {
                    let width = rooms_core::CAPTURE_THUMBNAIL_MAX_SIDE;
                    let height = (width * 9 / 16).max(2);
                    let bgra =
                        rooms_core::TestPatternSource::render(width, height, index as u64 * 12);
                    let frame = rooms_core::BgraFrame {
                        width,
                        height,
                        stride: width as usize * 4,
                        data: &bgra,
                    };
                    if let Ok(vf) = rooms_core::bgra_to_video_frame(&frame, width) {
                        let delay = if self.delays.reply.is_zero() {
                            Duration::ZERO
                        } else {
                            Duration::from_millis((index as u64 + 1) * 90)
                        };
                        self.schedule(
                            delay,
                            UiEvent::CaptureThumbnail {
                                id: target.id,
                                width: vf.width as usize,
                                height: vf.height as usize,
                                rgba: vf.rgba,
                            },
                        );
                    }
                }
            }
            UiCommand::CancelCaptureTargets => self.cancel_capture_targets(),
            UiCommand::StartScreenShare { target_id, .. } => self.start_share(&target_id),
            UiCommand::StopScreenShare => {
                if self.share_preview.take().is_some() {
                    self.schedule(Duration::ZERO, UiEvent::ScreenShareStopped);
                }
            }
            UiCommand::WatchScreenShare { identity } => self.watch_share(identity),
            UiCommand::StopWatching => {
                if self.watch.take().is_some() {
                    self.schedule(Duration::ZERO, UiEvent::WatchingStopped);
                }
            }
            UiCommand::SetUiVisible(_) => {}
            UiCommand::RefreshRooms => {
                let rooms = self.rooms();
                self.schedule(Duration::ZERO, UiEvent::RoomsUpdated(rooms));
            }
        }
    }

    fn cancel_capture_targets(&mut self) {
        self.scheduled.retain(|s| {
            !matches!(
                s.event,
                UiEvent::CaptureTargetsUpdated(_) | UiEvent::CaptureThumbnail { .. }
            )
        });
    }

    fn publish_audio_devices(&mut self) {
        let event = self.audio_devices_event();
        self.schedule(Duration::ZERO, event);
    }

    fn start_share(&mut self, target_id: &str) {
        if self.room.is_none() || self.share_preview.is_some() {
            return;
        }
        self.cancel_capture_targets();
        if demo_targets().iter().any(|t| t.id == target_id) {
            let Some(feed) = TestFeed::start_with(480, 270, 10) else {
                return;
            };
            let preview = feed.handle();
            self.share_preview = Some(feed);
            self.schedule(Duration::ZERO, UiEvent::ScreenShareStarted { preview });
        } else {
            self.schedule(
                Duration::ZERO,
                UiEvent::ShareFailed {
                    message: "A janela ou tela escolhida não está mais disponível.".to_owned(),
                },
            );
        }
    }

    fn watch_share(&mut self, identity: String) {
        if !(self.room.is_some() && self.ana_sharing && identity == "demo-ana") {
            return;
        }
        self.watch = None;
        let Some(feed) = TestFeed::start() else {
            return;
        };
        let handle = feed.handle();
        self.watch = Some(feed);
        self.schedule(
            Duration::ZERO,
            UiEvent::WatchingStarted {
                identity,
                feed: handle,
            },
        );
    }

    fn join(&mut self, room: String) {
        self.reset_share();
        self.scheduled.clear();
        self.chat_counter = 0;
        self.file_counter = 0;
        if room == FAILING_ROOM {
            self.room = None;
            self.schedule(Duration::ZERO, UiEvent::Connecting);
            self.schedule(
                self.delays.join,
                UiEvent::Failed {
                    message: "Sala de demonstração que sempre falha.".to_owned(),
                },
            );
            return;
        }
        let mut me = Self::participant(SELF_IDENTITY, SELF_NAME);
        me.muted = self.muted;
        let ana = Self::participant("demo-ana", "Ana");
        let bruno = Self::participant("demo-bruno", "Bruno");
        self.participants = vec![me.clone(), ana.clone()];
        self.room = Some(room.clone());

        let join = self.delays.join;
        let late = join + self.delays.late_participant;
        self.schedule(Duration::ZERO, UiEvent::Connecting);
        self.schedule(
            join,
            UiEvent::Joined {
                room,
                local_identity: SELF_IDENTITY.to_owned(),
                participants: vec![me, ana],
            },
        );
        self.schedule(
            join,
            UiEvent::ChatHistory {
                messages: Self::initial_history(),
                has_more: true,
                before: None,
            },
        );
        self.ana_sharing = true;
        self.schedule(
            join + self.delays.remote_share,
            UiEvent::RemoteScreenShareStarted {
                identity: "demo-ana".to_owned(),
            },
        );
        self.schedule(late, UiEvent::ParticipantJoined(bruno.clone()));
        let speaking = ParticipantView {
            speaking: true,
            ..bruno.clone()
        };
        self.schedule(
            late + Duration::from_millis(600),
            UiEvent::ParticipantUpdated(speaking),
        );
        self.schedule(
            late + Duration::from_millis(2400),
            UiEvent::ParticipantUpdated(bruno),
        );
        self.schedule(
            late + Duration::from_secs(6),
            UiEvent::ParticipantLeft {
                identity: "demo-bruno".to_owned(),
            },
        );
        self.schedule(late + Duration::from_secs(9), UiEvent::Reconnecting);
        self.schedule(late + Duration::from_secs(11), UiEvent::Reconnected);
    }

    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if self.room.is_none() {
            return;
        }
        if let Some(me) = self
            .participants
            .iter_mut()
            .find(|p| p.identity == SELF_IDENTITY)
        {
            me.muted = muted;
            let update = UiEvent::ParticipantUpdated(me.clone());
            self.schedule(Duration::ZERO, update);
        }
    }

    fn chat(&mut self, text: String, save: bool) {
        if self.room.is_none() {
            return;
        }
        self.chat_counter += 1;
        let echo = Self::chat_message(
            format!("demo-{}", self.chat_counter * 2 - 1),
            SELF_IDENTITY,
            SELF_NAME,
            text,
            save,
            Self::now_unix_ms(),
        );
        self.schedule(Duration::ZERO, UiEvent::ChatReceived(echo));
        if self.chat_counter.is_multiple_of(2) {
            let index = usize::try_from(self.chat_counter / 2).unwrap_or(0) % REPLIES.len();
            let reply = Self::chat_message(
                format!("demo-{}", self.chat_counter * 2),
                "demo-ana",
                "Ana",
                REPLIES[index].to_owned(),
                false,
                Self::now_unix_ms(),
            );
            self.schedule(self.delays.reply, UiEvent::ChatReceived(reply));
        }
    }

    fn load_chat_history(&mut self, before: Option<String>) {
        if self.room.is_none() {
            return;
        }
        let delay = self.delays.reply.min(Duration::from_millis(250));
        let event = match before {
            Some(cursor) => UiEvent::ChatHistory {
                messages: Self::older_history(),
                has_more: false,
                before: Some(cursor),
            },
            None => UiEvent::ChatHistory {
                messages: Self::initial_history(),
                has_more: true,
                before: None,
            },
        };
        self.schedule(delay, event);
    }

    fn delete_chat(&mut self, id: String) {
        if self.room.is_none() {
            return;
        }
        self.schedule(
            Duration::ZERO,
            UiEvent::ChatDeleted {
                id,
                sender: SELF_IDENTITY.to_owned(),
            },
        );
    }

    fn send_file(&mut self, path: PathBuf) {
        if self.room.is_none() {
            return;
        }
        self.file_counter += 1;
        let size = std::fs::metadata(&path).map_or(1024 * 1024, |m| m.len());
        let name = path.file_name().map_or_else(
            || "arquivo".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let sent_at_unix_ms = Self::now_unix_ms();
        let view = |state| FileTransferView {
            id: format!("demo-file-{}", self.file_counter),
            sender: SELF_NAME.to_owned(),
            name: name.clone(),
            size,
            is_image: is_image_name(&name),
            outgoing: true,
            sent_at_unix_ms,
            state,
        };
        let started = view(TransferStateView::InProgress { bytes: size / 2 });
        let finished = view(TransferStateView::Done { path });
        self.schedule(Duration::ZERO, UiEvent::FileTransfer(started));
        self.schedule(self.delays.reply, UiEvent::FileTransfer(finished));
    }

    fn now_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }

    fn chat_message(
        id: String,
        sender_id: &str,
        sender: &str,
        text: String,
        saved: bool,
        sent_at_unix_ms: u64,
    ) -> ChatMessageView {
        ChatMessageView {
            id,
            sender_id: sender_id.to_owned(),
            sender: sender.to_owned(),
            sent_at_unix_ms,
            text,
            saved,
        }
    }

    fn initial_history() -> Vec<ChatMessageView> {
        let now = Self::now_unix_ms();
        vec![
            Self::chat_message(
                "demo-hist-3".to_owned(),
                "demo-ana",
                "Ana",
                "Alguém topa chamada mais tarde?".to_owned(),
                true,
                now.saturating_sub(30 * 60_000),
            ),
            Self::chat_message(
                "demo-hist-4".to_owned(),
                SELF_IDENTITY,
                SELF_NAME,
                "Link da pauta já está no chat.".to_owned(),
                true,
                now.saturating_sub(15 * 60_000),
            ),
        ]
    }

    fn older_history() -> Vec<ChatMessageView> {
        let now = Self::now_unix_ms();
        vec![
            Self::chat_message(
                "demo-hist-1".to_owned(),
                SELF_IDENTITY,
                SELF_NAME,
                "Bem-vindo à sala de demonstração.".to_owned(),
                true,
                now.saturating_sub(120 * 60_000),
            ),
            Self::chat_message(
                "demo-hist-2".to_owned(),
                "demo-ana",
                "Ana",
                "Combinado!".to_owned(),
                true,
                now.saturating_sub(60 * 60_000),
            ),
        ]
    }

    fn rooms(&self) -> Vec<RoomSummary> {
        let fixed = [
            ("geral", vec!["Ana", "Bruno"]),
            ("jogos", vec!["Carla"]),
            ("musica", vec![]),
        ];
        fixed
            .into_iter()
            .map(|(name, occupants)| {
                let participants = if self.room.as_deref() == Some(name) {
                    self.participants.iter().map(|p| p.name.clone()).collect()
                } else {
                    occupants.into_iter().map(str::to_owned).collect()
                };
                RoomSummary {
                    name: name.to_owned(),
                    title: String::new(),
                    participants,
                }
            })
            .collect()
    }
}

impl Default for DemoSession {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionPort for DemoSession {
    fn send(&self, command: UiCommand) {
        if self.commands_tx.send(command).is_err() {
            tracing::warn!("demo session command channel closed");
        }
    }

    fn try_recv(&mut self) -> Option<UiEvent> {
        while let Ok(command) = self.commands_rx.try_recv() {
            self.handle(command);
        }
        let now = Instant::now();
        let index = self
            .scheduled
            .iter()
            .enumerate()
            .filter(|(_, s)| s.due <= now)
            .min_by_key(|(_, s)| (s.due, s.seq))
            .map(|(index, _)| index)?;
        Some(self.scheduled.swap_remove(index).event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(session: &mut DemoSession) -> Vec<UiEvent> {
        std::iter::from_fn(|| session.try_recv()).collect()
    }

    fn joined(session: &mut DemoSession) -> Vec<UiEvent> {
        session.send(UiCommand::Join {
            room: "geral".to_owned(),
        });
        drain(session)
    }

    #[test]
    fn join_yields_connecting_then_joined() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        let events = joined(&mut session);
        assert_eq!(events.first(), Some(&UiEvent::Connecting));
        assert!(events.iter().any(|e| matches!(
            e,
            UiEvent::Joined { room, local_identity, participants }
                if room == "geral" && local_identity == SELF_IDENTITY && participants.len() == 2
        )));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, UiEvent::ParticipantJoined(p) if p.name == "Bruno"))
        );
    }

    #[test]
    fn joining_the_failing_room_reports_failure() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        session.send(UiCommand::Join {
            room: FAILING_ROOM.to_owned(),
        });
        let events = drain(&mut session);
        assert!(matches!(events.last(), Some(UiEvent::Failed { .. })));
    }

    #[test]
    fn join_is_delayed_with_default_delays() {
        let mut session = DemoSession::new();
        session.send(UiCommand::Join {
            room: "geral".to_owned(),
        });
        let events = drain(&mut session);
        assert_eq!(events, vec![UiEvent::Connecting]);
    }

    #[test]
    fn chat_is_echoed_and_every_second_message_gets_a_reply() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        joined(&mut session);
        session.send(UiCommand::SendChat {
            text: "oi".to_owned(),
            save: false,
        });
        let first = drain(&mut session);
        assert!(matches!(
            first.as_slice(),
            [UiEvent::ChatReceived(m)] if m.text == "oi" && m.sender == SELF_NAME && !m.saved
        ));
        session.send(UiCommand::SendChat {
            text: "tudo bem?".to_owned(),
            save: false,
        });
        let second = drain(&mut session);
        assert_eq!(second.len(), 2);
        assert!(matches!(&second[1], UiEvent::ChatReceived(m) if m.sender == "Ana"));
    }

    #[test]
    fn sent_file_goes_from_progress_to_done() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        joined(&mut session);
        session.send(UiCommand::SendFile {
            path: PathBuf::from("/nonexistent/foto.png"),
        });
        let events = drain(&mut session);
        assert!(matches!(
            events.as_slice(),
            [
                UiEvent::FileTransfer(a),
                UiEvent::FileTransfer(b),
            ] if a.id == b.id
                && a.is_image
                && a.outgoing
                && matches!(a.state, TransferStateView::InProgress { .. })
                && matches!(&b.state, TransferStateView::Done { path } if path.ends_with("foto.png"))
        ));
    }

    #[test]
    fn file_outside_a_room_is_ignored() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        session.send(UiCommand::SendFile {
            path: PathBuf::from("a.png"),
        });
        assert!(drain(&mut session).is_empty());
    }

    #[test]
    fn chat_outside_a_room_is_ignored() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        session.send(UiCommand::SendChat {
            text: "oi".to_owned(),
            save: false,
        });
        assert!(drain(&mut session).is_empty());
    }

    #[test]
    fn refresh_returns_three_rooms_with_live_occupants() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        joined(&mut session);
        session.send(UiCommand::RefreshRooms);
        let events = drain(&mut session);
        let Some(UiEvent::RoomsUpdated(rooms)) = events.first() else {
            panic!("expected RoomsUpdated, got {events:?}");
        };
        assert_eq!(rooms.len(), 3);
        let geral = &rooms[0];
        assert!(geral.participants.contains(&SELF_NAME.to_owned()));
    }

    #[test]
    fn set_muted_updates_own_participant() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        joined(&mut session);
        session.send(UiCommand::SetMuted(true));
        let events = drain(&mut session);
        assert!(matches!(
            events.as_slice(),
            [UiEvent::ParticipantUpdated(p)] if p.identity == SELF_IDENTITY && p.muted
        ));
    }

    #[test]
    fn audio_devices_are_listed_and_selection_is_echoed() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        session.send(UiCommand::RefreshAudioDevices);
        let events = drain(&mut session);
        let [
            UiEvent::AudioDevicesUpdated {
                inputs,
                outputs,
                selected_input: None,
                selected_output: None,
            },
        ] = events.as_slice()
        else {
            panic!("unexpected events: {events:?}");
        };
        assert_eq!(inputs.len(), 2);
        assert_eq!(outputs.len(), 2);

        session.send(UiCommand::SetOutputDevice(Some("demo-out-1".to_owned())));
        let events = drain(&mut session);
        assert!(matches!(
            events.as_slice(),
            [UiEvent::AudioDevicesUpdated { selected_output: Some(id), .. }] if id == "demo-out-1"
        ));
    }

    #[test]
    fn capture_targets_are_listed() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        session.send(UiCommand::RefreshCaptureTargets);
        let events = drain(&mut session);
        assert!(matches!(
            events.first(),
            Some(UiEvent::CaptureTargetsUpdated(t)) if t.len() == 4
        ));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, UiEvent::CaptureThumbnail { .. }))
                .count(),
            4
        );
    }

    #[test]
    fn local_share_starts_stops_and_rejects_unknown_targets() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        joined(&mut session);
        let start = |id: &str| UiCommand::StartScreenShare {
            target_id: id.to_owned(),
            preset: crate::ui_model::SharePresetView::default(),
        };
        session.send(start("nope"));
        assert!(matches!(
            drain(&mut session).as_slice(),
            [UiEvent::ShareFailed { .. }]
        ));
        session.send(start("demo:screen:0"));
        let events = drain(&mut session);
        let [UiEvent::ScreenShareStarted { preview }] = events.as_slice() else {
            panic!("unexpected events: {events:?}");
        };
        let mut seen = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while preview.take_newer(&mut seen).is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(seen > 0);
        session.send(start("demo:screen:1"));
        assert!(drain(&mut session).is_empty());
        session.send(UiCommand::StopScreenShare);
        assert_eq!(drain(&mut session), vec![UiEvent::ScreenShareStopped]);
    }

    #[test]
    fn ana_shares_and_can_be_watched_until_stop_or_leave() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        let events = joined(&mut session);
        assert!(events.iter().any(|e| matches!(
            e,
            UiEvent::RemoteScreenShareStarted { identity } if identity == "demo-ana"
        )));
        session.send(UiCommand::WatchScreenShare {
            identity: "demo-ana".to_owned(),
        });
        let events = drain(&mut session);
        let [UiEvent::WatchingStarted { identity, feed }] = events.as_slice() else {
            panic!("unexpected events: {events:?}");
        };
        assert_eq!(identity, "demo-ana");
        let mut seen = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while feed.take_newer(&mut seen).is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(seen > 0);
        session.send(UiCommand::StopWatching);
        assert_eq!(drain(&mut session), vec![UiEvent::WatchingStopped]);
        session.send(UiCommand::WatchScreenShare {
            identity: "demo-bruno".to_owned(),
        });
        assert!(drain(&mut session).is_empty());
    }

    #[test]
    fn leave_emits_left_and_drops_pending_events() {
        let mut session = DemoSession::with_delays(DemoDelays::default());
        session.send(UiCommand::Join {
            room: "geral".to_owned(),
        });
        session.send(UiCommand::Leave);
        assert_eq!(drain(&mut session), vec![UiEvent::Left]);
    }

    #[test]
    fn demo_supports_saved_messages_history_pages_and_deletion() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        let events = joined(&mut session);
        assert!(events.iter().any(|e| matches!(
            e,
            UiEvent::ChatHistory {
                messages,
                has_more: true,
                before: None,
            } if !messages.is_empty() && messages.iter().all(|m| m.saved)
        )));

        session.send(UiCommand::SendChat {
            text: "salva".to_owned(),
            save: true,
        });
        assert!(matches!(
            drain(&mut session).as_slice(),
            [UiEvent::ChatReceived(m)] if m.saved && m.sender_id == SELF_IDENTITY && m.text == "salva"
        ));

        session.send(UiCommand::LoadChatHistory {
            before: Some("demo-hist-3".to_owned()),
        });
        assert!(matches!(
            drain(&mut session).as_slice(),
            [UiEvent::ChatHistory {
                messages,
                has_more: false,
                before: Some(cursor),
            }] if !messages.is_empty() && cursor == "demo-hist-3"
        ));

        session.send(UiCommand::DeleteChat {
            id: "demo-hist-4".to_owned(),
        });
        assert_eq!(
            drain(&mut session),
            vec![UiEvent::ChatDeleted {
                id: "demo-hist-4".to_owned(),
                sender: SELF_IDENTITY.to_owned(),
            }]
        );
    }

    #[test]
    fn participant_volume_command_is_accepted_without_effect() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        session.send(UiCommand::SetParticipantVolume {
            identity: "demo-ana".to_owned(),
            percent: 60,
        });
        assert!(drain(&mut session).is_empty());
    }

    #[test]
    fn cancel_capture_targets_drops_scheduled_thumbnails_and_ui_visible_is_accepted() {
        let mut session = DemoSession::with_delays(DemoDelays::instant());
        session.send(UiCommand::RefreshCaptureTargets);
        session.send(UiCommand::CancelCaptureTargets);
        session.send(UiCommand::SetUiVisible(false));
        session.send(UiCommand::SetUiVisible(true));
        assert!(drain(&mut session).is_empty());
    }
}
