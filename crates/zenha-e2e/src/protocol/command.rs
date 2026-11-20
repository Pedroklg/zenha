//! Commands the orchestrator sends to the bot and their parsing.

use serde::{Deserialize, Serialize};
use zenha_core::{
    AdminAction, Command, Cue, NoiseSuppression, SharePreset, TEST_PATTERN_TARGET_ID,
};

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
    #[serde(alias = "cancel-capture-targets")]
    CancelCaptureTargets,
    #[serde(alias = "set-ui-visible")]
    SetUiVisible {
        visible: bool,
    },
    #[serde(alias = "measure-frames")]
    MeasureFrames {
        duration_ms: u64,
    },
    #[serde(alias = "play-cue")]
    PlayCue {
        cue: String,
    },
    #[serde(alias = "admin-login")]
    AdminLogin {
        password: String,
    },
    #[serde(alias = "admin-logout")]
    AdminLogout,
    #[serde(alias = "admin-create-room")]
    AdminCreateRoom {
        title: String,
    },
    #[serde(alias = "admin-update-room")]
    AdminUpdateRoom {
        id: String,
        title: Option<String>,
        position: Option<usize>,
    },
    #[serde(alias = "admin-delete-room")]
    AdminDeleteRoom {
        id: String,
    },
    #[serde(alias = "admin-mute-participant")]
    AdminMuteParticipant {
        room: String,
        identity: String,
    },
    #[serde(alias = "admin-stop-share-participant")]
    AdminStopShareParticipant {
        room: String,
        identity: String,
    },
    #[serde(alias = "admin-kick-participant")]
    AdminKickParticipant {
        room: String,
        identity: String,
    },
    #[serde(alias = "admin-move-participant")]
    AdminMoveParticipant {
        room: String,
        identity: String,
        to_room: String,
    },
    #[serde(alias = "admin-delete-chat")]
    AdminDeleteChat {
        room: String,
        id: String,
    },
    #[serde(alias = "admin-rotate-invite")]
    AdminRotateInvite {
        new_code: String,
    },
    Quit,
}

fn parse_cue(name: &str) -> Option<Cue> {
    match name {
        "muted" => Some(Cue::Muted),
        "unmuted" => Some(Cue::Unmuted),
        "deafened" => Some(Cue::Deafened),
        "undeafened" => Some(Cue::Undeafened),
        "self_joined" => Some(Cue::SelfJoined),
        "self_left" => Some(Cue::SelfLeft),
        "peer_joined" => Some(Cue::PeerJoined),
        "peer_left" => Some(Cue::PeerLeft),
        "integration_post" => Some(Cue::IntegrationPost),
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
            Self::CancelCaptureTargets => Command::CancelCaptureTargets,
            Self::SetUiVisible { visible } => Command::SetUiVisible(*visible),
            Self::PlayCue { cue } => Command::PlayCue(parse_cue(cue)?),
            Self::AdminLogin { password } => Command::AdminLogin {
                password: password.clone(),
            },
            Self::AdminLogout => Command::AdminLogout,
            Self::AdminCreateRoom { title } => Command::Admin(AdminAction::CreateRoom {
                title: title.clone(),
                password: None,
                parent: None,
            }),
            Self::AdminUpdateRoom {
                id,
                title,
                position,
            } => Command::Admin(AdminAction::UpdateRoom {
                id: id.clone(),
                title: title.clone(),
                position: *position,
                password: None,
                parent: None,
            }),
            Self::AdminDeleteRoom { id } => {
                Command::Admin(AdminAction::DeleteRoom { id: id.clone() })
            }
            Self::AdminMuteParticipant { room, identity } => {
                Command::Admin(AdminAction::MuteParticipant {
                    room: room.clone(),
                    identity: identity.clone(),
                })
            }
            Self::AdminStopShareParticipant { room, identity } => {
                Command::Admin(AdminAction::StopShareParticipant {
                    room: room.clone(),
                    identity: identity.clone(),
                })
            }
            Self::AdminKickParticipant { room, identity } => {
                Command::Admin(AdminAction::KickParticipant {
                    room: room.clone(),
                    identity: identity.clone(),
                })
            }
            Self::AdminMoveParticipant {
                room,
                identity,
                to_room,
            } => Command::Admin(AdminAction::MoveParticipant {
                room: room.clone(),
                identity: identity.clone(),
                to_room: to_room.clone(),
            }),
            Self::AdminDeleteChat { room, id } => Command::Admin(AdminAction::DeleteChat {
                room: room.clone(),
                id: id.clone(),
            }),
            Self::AdminRotateInvite { new_code } => Command::Admin(AdminAction::RotateInvite {
                new_code: new_code.clone(),
            }),
            Self::MeasureFrames { .. } | Self::Quit => return None,
        })
    }
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
