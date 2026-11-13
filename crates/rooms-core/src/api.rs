use std::fmt;
use std::path::PathBuf;

use rooms_protocol::{ChatMessage, RoomSummary};

use crate::capture::CaptureEnd;
use crate::cue::Cue;
use crate::video::{CaptureTarget, FrameSlot, SharePreset};

#[derive(Clone, PartialEq, Eq)]
pub struct SessionConfig {
    pub token_server_url: String,
    pub client_id: String,
    pub display_name: String,
    pub invite_code: String,
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionConfig")
            .field("token_server_url", &self.token_server_url)
            .field("client_id", &self.client_id)
            .field("display_name", &self.display_name)
            .field("invite_code", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub identity: String,
    pub name: String,
    pub muted: bool,
    pub speaking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// Tratamento de ruído do microfone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoiseSuppression {
    Off,
    /// Supressão embutida do WebRTC.
    #[default]
    Standard,
    /// Rede neural (RNNoise) num caminho de captura próprio; sem cancelamento de eco.
    Enhanced,
    /// Rede DeepFilterNet 3 no mesmo caminho de captura próprio do modo Reforçada; mais pesada.
    Maximum,
}

impl NoiseSuppression {
    pub fn uses_own_capture(self) -> bool {
        matches!(self, Self::Enhanced | Self::Maximum)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransfer {
    pub id: String,
    /// Identidade de quem envia (como em `ChatMessage::sender` recebido).
    pub sender: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub outgoing: bool,
    pub sent_at_unix_ms: u64,
    pub state: TransferState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    InProgress {
        bytes: u64,
    },
    /// Onde o arquivo está neste computador: o original (envio) ou a cópia recebida.
    Done {
        path: PathBuf,
    },
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDevices {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
    pub selected_input: Option<String>,
    pub selected_output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Join {
        room: String,
    },
    Leave,
    SetMuted(bool),
    SetDeafened(bool),
    SendChat {
        text: String,
    },
    SendFile {
        path: PathBuf,
    },
    RefreshRooms,
    RefreshAudioDevices,
    SetInputDevice(Option<String>),
    SetOutputDevice(Option<String>),
    SetNoiseSuppression(NoiseSuppression),
    /// Volume do som da transmissão assistida, de 0 a 100.
    SetStreamVolume(u8),
    /// Limiar da porta de ruído em dBFS, de -70 a -20; `None` a desliga.
    SetNoiseGate(Option<i8>),
    RefreshCaptureTargets,
    StartScreenShare {
        target_id: String,
        preset: SharePreset,
    },
    StopScreenShare,
    WatchScreenShare {
        identity: String,
    },
    StopWatching,
    /// Som curto de confirmação, só para quem usa o app.
    PlayCue(Cue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connecting,
    Joined {
        room: String,
        local_identity: String,
        participants: Vec<Participant>,
    },
    Reconnecting,
    Reconnected,
    Left,
    Failed(SessionError),
    ParticipantList(Vec<Participant>),
    ParticipantJoined(Participant),
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
    ChatReceived(ChatMessage),
    FileTransfer(FileTransfer),
    RoomsUpdated(Vec<RoomSummary>),
    AudioDevicesUpdated(AudioDevices),
    CaptureTargetsUpdated(Vec<CaptureTarget>),
    /// Nível do microfone no modo Reforçada, cerca de dez vezes por segundo.
    MicLevel {
        level_db: i8,
        gate_open: bool,
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
        frames: FrameSlot,
    },
    WatchingStopped,
    CommandFailed(SessionError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("could not reach the token server: {0}")]
    TokenServer(String),
    #[error("invite code rejected")]
    InviteRejected,
    #[error("connection to the media server failed: {0}")]
    Connection(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not in a room")]
    NotInRoom,
    #[error("screen sharing is not available in this build")]
    ShareUnavailable,
    #[error("a screen share is already active")]
    ShareActive,
    #[error("capture target not found: {0}")]
    TargetNotFound(String),
    #[error("screen capture ended: {0}")]
    CaptureEnded(CaptureEnd),
    #[error("video error: {0}")]
    Video(String),
    #[error("internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_enhanced_and_maximum_use_their_own_capture() {
        assert!(!NoiseSuppression::Off.uses_own_capture());
        assert!(!NoiseSuppression::Standard.uses_own_capture());
        assert!(NoiseSuppression::Enhanced.uses_own_capture());
        assert!(NoiseSuppression::Maximum.uses_own_capture());
    }
}
