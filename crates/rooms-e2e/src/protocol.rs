//! Protocolo de linhas JSON entre o orquestrador e os bots: comandos no stdin, eventos no stdout.

use rooms_core::{
    AudioDevice, AudioDevices, CaptureKind, CaptureTarget, Command, Cue, Event, NoiseSuppression,
    Participant, SessionError, SharePreset, TEST_PATTERN_TARGET_ID, TransferState,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseKind {
    Off,
    Standard,
    Enhanced,
    Maximum,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum BotCommand {
    Join {
        room: String,
    },
    Leave,
    #[serde(alias = "set-muted")]
    SetMuted {
        muted: bool,
    },
    #[serde(alias = "set-deafened")]
    SetDeafened {
        deafened: bool,
    },
    #[serde(alias = "send-chat")]
    SendChat {
        text: String,
    },
    #[serde(alias = "send-file")]
    SendFile {
        path: String,
    },
    #[serde(alias = "set-output-device")]
    SetOutputDevice {
        id: Option<String>,
    },
    #[serde(alias = "set-input-device")]
    SetInputDevice {
        id: Option<String>,
    },
    #[serde(alias = "set-noise-suppression")]
    SetNoiseSuppression {
        mode: NoiseKind,
    },
    #[serde(alias = "set-stream-volume")]
    SetStreamVolume {
        percent: u8,
    },
    #[serde(alias = "set-participant-volume")]
    SetParticipantVolume {
        identity: String,
        percent: u16,
    },
    #[serde(alias = "set-noise-gate")]
    SetNoiseGate {
        db: Option<i8>,
    },
    #[serde(alias = "refresh-devices")]
    RefreshDevices,
    #[serde(alias = "start-share")]
    StartShare,
    #[serde(alias = "stop-share")]
    StopShare,
    Watch {
        identity: String,
    },
    #[serde(alias = "stop-watching")]
    StopWatching,
    #[serde(alias = "measure-frames")]
    MeasureFrames {
        duration_ms: u64,
    },
    #[serde(alias = "play-cue")]
    PlayCue {
        cue: String,
    },
    Quit,
}

fn parse_cue(name: &str) -> Option<Cue> {
    match name {
        "muted" => Some(Cue::Muted),
        "unmuted" => Some(Cue::Unmuted),
        "deafened" => Some(Cue::Deafened),
        "undeafened" => Some(Cue::Undeafened),
        _ => None,
    }
}

impl BotCommand {
    /// The session command, or `None` for commands the bot handles itself.
    pub fn to_core(&self) -> Option<Command> {
        Some(match self {
            Self::Join { room } => Command::Join { room: room.clone() },
            Self::Leave => Command::Leave,
            Self::SetMuted { muted } => Command::SetMuted(*muted),
            Self::SetDeafened { deafened } => Command::SetDeafened(*deafened),
            Self::SendChat { text } => Command::SendChat {
                text: text.clone(),
                save: false,
            },
            Self::SendFile { path } => Command::SendFile { path: path.into() },
            Self::SetOutputDevice { id } => Command::SetOutputDevice(id.clone()),
            Self::SetInputDevice { id } => Command::SetInputDevice(id.clone()),
            Self::SetNoiseSuppression { mode } => Command::SetNoiseSuppression(match mode {
                NoiseKind::Off => NoiseSuppression::Off,
                NoiseKind::Standard => NoiseSuppression::Standard,
                NoiseKind::Enhanced => NoiseSuppression::Enhanced,
                NoiseKind::Maximum => NoiseSuppression::Maximum,
            }),
            Self::SetStreamVolume { percent } => Command::SetStreamVolume(*percent),
            Self::SetParticipantVolume { identity, percent } => Command::SetParticipantVolume {
                identity: identity.clone(),
                percent: *percent,
            },
            Self::SetNoiseGate { db } => Command::SetNoiseGate(*db),
            Self::RefreshDevices => Command::RefreshAudioDevices,
            Self::StartShare => Command::StartScreenShare {
                target_id: TEST_PATTERN_TARGET_ID.to_owned(),
                preset: SharePreset::P720F30,
            },
            Self::StopShare => Command::StopScreenShare,
            Self::Watch { identity } => Command::WatchScreenShare {
                identity: identity.clone(),
            },
            Self::StopWatching => Command::StopWatching,
            Self::PlayCue { cue } => Command::PlayCue(parse_cue(cue)?),
            Self::MeasureFrames { .. } | Self::Quit => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipantInfo {
    pub identity: String,
    pub name: String,
    pub muted: bool,
    pub speaking: bool,
}

impl From<&Participant> for ParticipantInfo {
    fn from(p: &Participant) -> Self {
        Self {
            identity: p.identity.clone(),
            name: p.name.clone(),
            muted: p.muted,
            speaking: p.speaking,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

impl From<&AudioDevice> for DeviceInfo {
    fn from(d: &AudioDevice) -> Self {
        Self {
            id: d.id.clone(),
            name: d.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
}

impl From<&CaptureTarget> for TargetInfo {
    fn from(t: &CaptureTarget) -> Self {
        Self {
            id: t.id.clone(),
            name: t.name.clone(),
            kind: match t.kind {
                CaptureKind::Screen => "screen",
                CaptureKind::Window => "window",
            }
            .to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub name: String,
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub kind: String,
    pub message: String,
}

pub fn error_kind(error: &SessionError) -> &'static str {
    match error {
        SessionError::TokenServer(_) => "token_server",
        SessionError::InviteRejected => "invite_rejected",
        SessionError::Unauthorized => "unauthorized",
        SessionError::RateLimited => "rate_limited",
        SessionError::Chat(_) => "chat",
        SessionError::Connection(_) => "connection",
        SessionError::InvalidInput(_) => "invalid_input",
        SessionError::NotInRoom => "not_in_room",
        SessionError::ShareUnavailable => "share_unavailable",
        SessionError::ShareActive => "share_active",
        SessionError::TargetNotFound(_) => "target_not_found",
        SessionError::CaptureEnded(_) => "capture_ended",
        SessionError::Video(_) => "video",
        SessionError::Internal(_) => "internal",
    }
}

impl From<&SessionError> for ErrorInfo {
    fn from(error: &SessionError) -> Self {
        Self {
            kind: error_kind(error).to_owned(),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BotEvent {
    Ready {
        pid: u32,
        name: String,
        microphone: String,
        test_pattern: bool,
    },
    Connecting,
    Joined {
        room: String,
        local_identity: String,
        participants: Vec<ParticipantInfo>,
    },
    Reconnecting,
    Reconnected,
    Left,
    Failed {
        error: ErrorInfo,
    },
    ParticipantList {
        participants: Vec<ParticipantInfo>,
    },
    ParticipantJoined {
        participant: ParticipantInfo,
    },
    ParticipantLeft {
        identity: String,
    },
    ParticipantMuteChanged {
        identity: String,
        muted: bool,
    },
    ParticipantSpeakingChanged {
        identity: String,
        speaking: bool,
    },
    ChatReceived {
        id: String,
        sender: String,
        text: String,
        sent_at_unix_ms: u64,
    },
    FileTransfer {
        id: String,
        sender: String,
        name: String,
        size: u64,
        mime: String,
        outgoing: bool,
        /// "in_progress", "done" or "failed".
        state: String,
        bytes: Option<u64>,
        path: Option<String>,
        error: Option<String>,
    },
    RoomsUpdated {
        rooms: Vec<RoomInfo>,
    },
    AudioDevicesUpdated {
        inputs: Vec<DeviceInfo>,
        outputs: Vec<DeviceInfo>,
        selected_input: Option<String>,
        selected_output: Option<String>,
    },
    CaptureTargetsUpdated {
        targets: Vec<TargetInfo>,
    },
    ScreenShareStarted,
    ScreenShareStopped,
    RemoteScreenShareStarted {
        identity: String,
    },
    RemoteScreenShareStopped {
        identity: String,
    },
    WatchingStarted {
        identity: String,
    },
    WatchingStopped,
    CommandFailed {
        error: ErrorInfo,
    },
    FrameMeasurement {
        identity: String,
        frames: u64,
        seconds: f64,
        fps: f64,
        width: u32,
        height: u32,
        mean_rgb: [f64; 3],
        luma_stddev: f64,
    },
    FrameMeasurementFailed {
        reason: String,
    },
    BadCommand {
        line: String,
        reason: String,
    },
    Bye,
}

impl BotEvent {
    /// `None` for events that are too frequent to be worth logging, such as the mic level.
    pub fn from_core(event: &Event) -> Option<Self> {
        let people = |list: &[Participant]| list.iter().map(ParticipantInfo::from).collect();
        Some(match event {
            Event::MicLevel { .. } | Event::CaptureThumbnail { .. } => return None,
            Event::Connecting => Self::Connecting,
            Event::Joined {
                room,
                local_identity,
                participants,
            } => Self::Joined {
                room: room.clone(),
                local_identity: local_identity.clone(),
                participants: people(participants),
            },
            Event::Reconnecting => Self::Reconnecting,
            Event::Reconnected => Self::Reconnected,
            Event::Left => Self::Left,
            Event::Failed(error) => Self::Failed {
                error: error.into(),
            },
            Event::ParticipantList(list) => Self::ParticipantList {
                participants: people(list),
            },
            Event::ParticipantJoined(p) => Self::ParticipantJoined {
                participant: p.into(),
            },
            Event::ParticipantLeft { identity } => Self::ParticipantLeft {
                identity: identity.clone(),
            },
            Event::ParticipantMuteChanged { identity, muted } => Self::ParticipantMuteChanged {
                identity: identity.clone(),
                muted: *muted,
            },
            Event::ParticipantSpeakingChanged { identity, speaking } => {
                Self::ParticipantSpeakingChanged {
                    identity: identity.clone(),
                    speaking: *speaking,
                }
            }
            Event::ChatReceived(message) => Self::ChatReceived {
                id: message.id.clone(),
                sender: message.sender.clone(),
                text: message.text.clone(),
                sent_at_unix_ms: message.sent_at_unix_ms,
            },
            Event::FileTransfer(transfer) => {
                let (state, bytes, path, error) = match &transfer.state {
                    TransferState::InProgress { bytes: 0 } => ("in_progress", Some(0), None, None),
                    TransferState::InProgress { .. } => return None,
                    TransferState::Done { path } => (
                        "done",
                        None,
                        Some(path.to_string_lossy().into_owned()),
                        None,
                    ),
                    TransferState::Failed(message) => ("failed", None, None, Some(message.clone())),
                };
                Self::FileTransfer {
                    id: transfer.id.clone(),
                    sender: transfer.sender.clone(),
                    name: transfer.name.clone(),
                    size: transfer.size,
                    mime: transfer.mime.clone(),
                    outgoing: transfer.outgoing,
                    state: state.to_owned(),
                    bytes,
                    path,
                    error,
                }
            }
            Event::RoomsUpdated(rooms) => Self::RoomsUpdated {
                rooms: rooms
                    .iter()
                    .map(|room| RoomInfo {
                        name: room.name.clone(),
                        participants: room.participants.clone(),
                    })
                    .collect(),
            },
            Event::AudioDevicesUpdated(AudioDevices {
                inputs,
                outputs,
                selected_input,
                selected_output,
            }) => Self::AudioDevicesUpdated {
                inputs: inputs.iter().map(DeviceInfo::from).collect(),
                outputs: outputs.iter().map(DeviceInfo::from).collect(),
                selected_input: selected_input.clone(),
                selected_output: selected_output.clone(),
            },
            Event::CaptureTargetsUpdated(targets) => Self::CaptureTargetsUpdated {
                targets: targets.iter().map(TargetInfo::from).collect(),
            },
            Event::ScreenShareStarted { .. } => Self::ScreenShareStarted,
            Event::ScreenShareStopped => Self::ScreenShareStopped,
            Event::RemoteScreenShareStarted { identity } => Self::RemoteScreenShareStarted {
                identity: identity.clone(),
            },
            Event::RemoteScreenShareStopped { identity } => Self::RemoteScreenShareStopped {
                identity: identity.clone(),
            },
            Event::WatchingStarted { identity, .. } => Self::WatchingStarted {
                identity: identity.clone(),
            },
            Event::WatchingStopped => Self::WatchingStopped,
            Event::ChatHistory { .. } | Event::ChatDeleted { .. } => return None,
            Event::CommandFailed(error) => Self::CommandFailed {
                error: error.into(),
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stamped {
    pub t_ms: u64,
    #[serde(flatten)]
    pub event: BotEvent,
}

pub fn parse_command(line: &str) -> Result<BotCommand, String> {
    let command: BotCommand = serde_json::from_str(line.trim()).map_err(|err| err.to_string())?;
    if let BotCommand::PlayCue { cue } = &command
        && parse_cue(cue).is_none()
    {
        return Err(format!("unknown cue: {cue}"));
    }
    Ok(command)
}

pub fn encode_event(t_ms: u64, event: &BotEvent) -> String {
    let stamped = Stamped {
        t_ms,
        event: event.clone(),
    };
    serde_json::to_string(&stamped).unwrap_or_else(|err| {
        format!(r#"{{"t_ms":{t_ms},"event":"bad_command","line":"","reason":"encode: {err}"}}"#)
    })
}

/// Returns `None` for lines that are not events, such as stray output of native libraries.
pub fn decode_event(line: &str) -> Option<Stamped> {
    serde_json::from_str(line.trim()).ok()
}

#[cfg(test)]
mod tests {
    use rooms_core::{FrameSlot, SessionError};
    use rooms_protocol::ChatMessage;

    use super::*;

    #[test]
    fn commands_parse_with_snake_and_kebab_names() {
        assert_eq!(
            parse_command(r#"{"cmd":"join","room":"x"}"#),
            Ok(BotCommand::Join { room: "x".into() })
        );
        assert_eq!(
            parse_command(r#"{"cmd":"set_muted","muted":true}"#),
            Ok(BotCommand::SetMuted { muted: true })
        );
        assert_eq!(
            parse_command(r#"{"cmd":"set-deafened","deafened":false}"#),
            Ok(BotCommand::SetDeafened { deafened: false })
        );
        assert_eq!(
            parse_command(r#"{"cmd":"set_output_device","id":null}"#),
            Ok(BotCommand::SetOutputDevice { id: None })
        );
        assert_eq!(
            parse_command(r#" {"cmd":"measure-frames","duration_ms":3000} "#),
            Ok(BotCommand::MeasureFrames { duration_ms: 3000 })
        );
        assert_eq!(
            parse_command(r#"{"cmd":"play_cue","cue":"muted"}"#),
            Ok(BotCommand::PlayCue {
                cue: "muted".into()
            })
        );
        assert_eq!(
            parse_command(r#"{"cmd":"play-cue","cue":"undeafened"}"#),
            Ok(BotCommand::PlayCue {
                cue: "undeafened".into()
            })
        );
        assert!(parse_command(r#"{"cmd":"play_cue","cue":"bogus"}"#).is_err());
        assert_eq!(parse_command(r#"{"cmd":"quit"}"#), Ok(BotCommand::Quit));
        assert!(parse_command(r#"{"cmd":"explode"}"#).is_err());
        assert!(parse_command("not json").is_err());
    }

    #[test]
    fn every_command_round_trips() {
        let all = [
            BotCommand::Join { room: "r".into() },
            BotCommand::Leave,
            BotCommand::SetMuted { muted: true },
            BotCommand::SetDeafened { deafened: true },
            BotCommand::SendChat { text: "oi".into() },
            BotCommand::SetOutputDevice {
                id: Some("d".into()),
            },
            BotCommand::SetInputDevice { id: None },
            BotCommand::RefreshDevices,
            BotCommand::StartShare,
            BotCommand::StopShare,
            BotCommand::Watch {
                identity: "t".into(),
            },
            BotCommand::StopWatching,
            BotCommand::MeasureFrames { duration_ms: 10 },
            BotCommand::PlayCue {
                cue: "muted".into(),
            },
            BotCommand::PlayCue {
                cue: "unmuted".into(),
            },
            BotCommand::PlayCue {
                cue: "deafened".into(),
            },
            BotCommand::PlayCue {
                cue: "undeafened".into(),
            },
            BotCommand::Quit,
        ];
        for command in all {
            let line = serde_json::to_string(&command).unwrap();
            assert_eq!(parse_command(&line), Ok(command));
        }
    }

    #[test]
    fn session_commands_map_and_local_ones_do_not() {
        assert_eq!(
            BotCommand::SetMuted { muted: true }.to_core(),
            Some(Command::SetMuted(true))
        );
        for (name, expected) in [
            ("muted", Cue::Muted),
            ("unmuted", Cue::Unmuted),
            ("deafened", Cue::Deafened),
            ("undeafened", Cue::Undeafened),
        ] {
            assert_eq!(
                BotCommand::PlayCue { cue: name.into() }.to_core(),
                Some(Command::PlayCue(expected))
            );
        }
        assert_eq!(BotCommand::PlayCue { cue: "nope".into() }.to_core(), None);
        assert!(matches!(
            BotCommand::StartShare.to_core(),
            Some(Command::StartScreenShare { target_id, .. }) if target_id == TEST_PATTERN_TARGET_ID
        ));
        assert_eq!(BotCommand::Quit.to_core(), None);
        assert_eq!(BotCommand::MeasureFrames { duration_ms: 1 }.to_core(), None);
    }

    #[test]
    fn events_round_trip_through_a_stamped_line() {
        let events = [
            BotEvent::Left,
            BotEvent::Joined {
                room: "r".into(),
                local_identity: "a".into(),
                participants: vec![ParticipantInfo {
                    identity: "a".into(),
                    name: "A".into(),
                    muted: false,
                    speaking: true,
                }],
            },
            BotEvent::Failed {
                error: ErrorInfo {
                    kind: "invite_rejected".into(),
                    message: "invite code rejected".into(),
                },
            },
            BotEvent::FrameMeasurement {
                identity: "t".into(),
                frames: 90,
                seconds: 3.0,
                fps: 30.0,
                width: 640,
                height: 360,
                mean_rgb: [1.0, 2.0, 3.0],
                luma_stddev: 4.0,
            },
            BotEvent::Bye,
        ];
        for event in events {
            let line = encode_event(1234, &event);
            assert!(!line.contains('\n'));
            let decoded = decode_event(&line).unwrap();
            assert_eq!(decoded.t_ms, 1234);
            assert_eq!(decoded.event, event);
        }
        assert!(decode_event("some native log line").is_none());
    }

    #[test]
    fn core_events_are_translated() {
        let chat = Event::ChatReceived(ChatMessage {
            id: "1".into(),
            sender: "t".into(),
            sent_at_unix_ms: 9,
            text: "oi".into(),
            saved: false,
        });
        assert_eq!(
            BotEvent::from_core(&chat),
            Some(BotEvent::ChatReceived {
                id: "1".into(),
                sender: "t".into(),
                text: "oi".into(),
                sent_at_unix_ms: 9
            })
        );
        let failed = Event::Failed(SessionError::InviteRejected);
        assert_eq!(
            BotEvent::from_core(&failed),
            Some(BotEvent::Failed {
                error: ErrorInfo {
                    kind: "invite_rejected".into(),
                    message: "invite code rejected".into()
                }
            })
        );
        let watching = Event::WatchingStarted {
            identity: "t".into(),
            frames: FrameSlot::new(),
        };
        assert_eq!(
            BotEvent::from_core(&watching),
            Some(BotEvent::WatchingStarted {
                identity: "t".into()
            })
        );
    }

    fn transfer(state: TransferState) -> Event {
        Event::FileTransfer(rooms_core::FileTransfer {
            id: "f1".into(),
            sender: "alice".into(),
            name: "a.png".into(),
            size: 10,
            mime: "image/png".into(),
            outgoing: false,
            sent_at_unix_ms: 1,
            state,
        })
    }

    #[test]
    fn file_transfer_events_log_start_and_end_only() {
        assert_eq!(
            parse_command(r#"{"cmd":"send-file","path":"/tmp/x"}"#),
            Ok(BotCommand::SendFile {
                path: "/tmp/x".into()
            })
        );
        assert_eq!(
            BotCommand::SendFile {
                path: "/tmp/x".into()
            }
            .to_core(),
            Some(Command::SendFile {
                path: "/tmp/x".into()
            })
        );
        let started = BotEvent::from_core(&transfer(TransferState::InProgress { bytes: 0 }));
        assert!(matches!(
            started,
            Some(BotEvent::FileTransfer { ref state, bytes: Some(0), .. }) if state == "in_progress"
        ));
        assert_eq!(
            BotEvent::from_core(&transfer(TransferState::InProgress { bytes: 5 })),
            None
        );
        let done = BotEvent::from_core(&transfer(TransferState::Done {
            path: "/r/f1-a.png".into(),
        }));
        assert!(matches!(
            done,
            Some(BotEvent::FileTransfer { ref state, path: Some(ref path), .. })
                if state == "done" && path == "/r/f1-a.png"
        ));
        let failed = BotEvent::from_core(&transfer(TransferState::Failed("x".into())));
        assert!(matches!(
            failed,
            Some(BotEvent::FileTransfer { ref state, error: Some(ref error), .. })
                if state == "failed" && error == "x"
        ));
    }

    #[test]
    fn mic_level_events_are_not_logged_and_noise_gate_commands_parse() {
        let level = Event::MicLevel {
            level_db: -40,
            gate_open: true,
        };
        assert_eq!(BotEvent::from_core(&level), None);
        assert_eq!(
            parse_command(r#"{"cmd":"set-noise-gate","db":-40}"#),
            Ok(BotCommand::SetNoiseGate { db: Some(-40) })
        );
        assert_eq!(
            parse_command(r#"{"cmd":"set_noise_gate","db":null}"#),
            Ok(BotCommand::SetNoiseGate { db: None })
        );
        assert_eq!(
            BotCommand::SetNoiseGate { db: Some(-40) }.to_core(),
            Some(Command::SetNoiseGate(Some(-40)))
        );
    }

    #[test]
    fn participant_volume_command_parses_and_translates() {
        assert_eq!(
            parse_command(r#"{"cmd":"set-participant-volume","identity":"alice","percent":70}"#),
            Ok(BotCommand::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 70,
            })
        );
        assert_eq!(
            parse_command(r#"{"cmd":"set_participant_volume","identity":"alice","percent":150}"#),
            Ok(BotCommand::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 150,
            })
        );
        assert_eq!(
            BotCommand::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 70,
            }
            .to_core(),
            Some(Command::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 70,
            })
        );
    }
}
