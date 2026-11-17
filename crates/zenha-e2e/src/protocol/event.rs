//! Eventos que o bot devolve, os tipos de informação que eles carregam e a codificação em linha.

use serde::{Deserialize, Serialize};
use zenha_core::{
    AudioDevice, AudioDevices, CaptureKind, CaptureTarget, Event, Participant, RemovalReason,
    SessionError, TransferState,
};

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
        SessionError::RoomLocked(_) => "room_locked",
        SessionError::RoomPasswordRejected => "room_password_rejected",
        SessionError::RoomUnlockRateLimited => "room_unlock_rate_limited",
        SessionError::Chat(_) => "chat",
        SessionError::Avatar(_) => "avatar",
        SessionError::AdminInvalidPassword => "admin_invalid_password",
        SessionError::AdminUnauthorized => "admin_unauthorized",
        SessionError::AdminDisabled => "admin_disabled",
        SessionError::AdminForbidden => "admin_forbidden",
        SessionError::AdminRateLimited => "admin_rate_limited",
        SessionError::AdminTargetNotFound(_) => "admin_target_not_found",
        SessionError::Admin(_) => "admin",
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
    AdminChanged {
        active: bool,
    },
    AdminRoomCreated {
        id: String,
        title: String,
        position: usize,
    },
    AdminRoomUpdated {
        id: String,
        title: String,
        position: usize,
    },
    AdminRoomDeleted {
        id: String,
    },
    AdminInviteRotated,
    AdminStoppedScreenShare,
    MovedByAdmin {
        to_room: String,
    },
    RemovedFromRoom {
        reason: String,
    },
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
        gap_p95_ms: f64,
        gap_max_ms: f64,
        stutters: u32,
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
            Event::AdminChanged { active } => Self::AdminChanged { active: *active },
            Event::AdminRoomCreated(room) => Self::AdminRoomCreated {
                id: room.id.clone(),
                title: room.title.clone(),
                position: room.position,
            },
            Event::AdminRoomUpdated(room) => Self::AdminRoomUpdated {
                id: room.id.clone(),
                title: room.title.clone(),
                position: room.position,
            },
            Event::AdminRoomDeleted { id } => Self::AdminRoomDeleted { id: id.clone() },
            Event::AdminInviteRotated => Self::AdminInviteRotated,
            Event::AdminStoppedScreenShare => Self::AdminStoppedScreenShare,
            Event::MovedByAdmin { to_room } => Self::MovedByAdmin {
                to_room: to_room.clone(),
            },
            Event::RemovedFromRoom { reason } => Self::RemovedFromRoom {
                reason: match reason {
                    RemovalReason::Kicked => "kicked".to_owned(),
                    RemovalReason::RoomDeleted => "room_deleted".to_owned(),
                },
            },
            Event::ChatHistory { .. }
            | Event::ChatDeleted { .. }
            | Event::ParticipantNameChanged { .. }
            | Event::ParticipantAvatarChanged { .. }
            | Event::LocalAvatarChanged { .. }
            | Event::AvatarCached { .. }
            | Event::RoomUnlocked { .. } => return None,
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
