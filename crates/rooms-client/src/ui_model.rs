use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use rooms_protocol::RoomSummary;

use crate::settings::NoiseMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Join {
        room: String,
    },
    Leave,
    SetMuted(bool),
    SetDeafened(bool),
    SendChat {
        text: String,
        save: bool,
    },
    LoadChatHistory {
        before: Option<String>,
    },
    DeleteChat {
        id: String,
    },
    SendFile {
        path: PathBuf,
    },
    RefreshRooms,
    RefreshAudioDevices,
    SetInputDevice(Option<String>),
    SetOutputDevice(Option<String>),
    SetNoiseSuppression(NoiseMode),
    SetStreamVolume(u8),
    SetParticipantVolume {
        identity: String,
        percent: u16,
    },
    SetNoiseGate(Option<i8>),
    RefreshCaptureTargets,
    StartScreenShare {
        target_id: String,
        preset: SharePresetView,
    },
    StopScreenShare,
    WatchScreenShare {
        identity: String,
    },
    StopWatching,
    PlayCue(rooms_core::Cue),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharePresetView {
    P720F30,
    #[default]
    P1080F30,
    P1080F60,
}

impl SharePresetView {
    pub const ALL: [SharePresetView; 3] = [Self::P720F30, Self::P1080F30, Self::P1080F60];

    pub fn label(self) -> &'static str {
        match self {
            Self::P720F30 => "720p, 30 fps",
            Self::P1080F30 => "1080p, 30 fps",
            Self::P1080F60 => "1080p, 60 fps (experimental)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKindView {
    Screen,
    Window,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTargetView {
    pub id: String,
    pub name: String,
    pub kind: TargetKindView,
}

/// One decoded video frame, RGBA8 tightly packed, shared without copying.
#[derive(Clone)]
pub struct UiFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<dyn AsRef<[u8]> + Send + Sync>,
}

impl UiFrame {
    pub fn bytes(&self) -> &[u8] {
        (*self.rgba).as_ref()
    }
}

pub trait FrameFeed: Send + Sync {
    /// Newest frame if newer than `*last_seen`; each frame is returned at most once.
    fn take_newer(&self, last_seen: &mut u64) -> Option<UiFrame>;
}

#[derive(Clone)]
pub struct FeedHandle(pub Arc<dyn FrameFeed>);

impl FeedHandle {
    pub fn take_newer(&self, last_seen: &mut u64) -> Option<UiFrame> {
        self.0.take_newer(last_seen)
    }
}

impl fmt::Debug for FeedHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FeedHandle")
    }
}

impl PartialEq for FeedHandle {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(Arc::as_ptr(&self.0), Arc::as_ptr(&other.0))
    }
}

impl Eq for FeedHandle {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceView {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantView {
    pub identity: String,
    pub name: String,
    pub muted: bool,
    pub speaking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStateView {
    InProgress { bytes: u64 },
    Done { path: PathBuf },
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransferView {
    pub id: String,
    pub sender: String,
    pub name: String,
    pub size: u64,
    pub is_image: bool,
    pub outgoing: bool,
    pub sent_at_unix_ms: u64,
    pub state: TransferStateView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessageView {
    pub id: String,
    pub sender_id: String,
    pub sender: String,
    pub sent_at_unix_ms: u64,
    pub text: String,
    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    Connecting,
    Joined {
        room: String,
        local_identity: String,
        participants: Vec<ParticipantView>,
    },
    ParticipantJoined(ParticipantView),
    ParticipantLeft {
        identity: String,
    },
    ParticipantUpdated(ParticipantView),
    Reconnecting,
    Reconnected,
    Left,
    Failed {
        message: String,
    },
    InviteRejected,
    RoomsFailed {
        message: String,
    },
    ChatReceived(ChatMessageView),
    ChatHistory {
        messages: Vec<ChatMessageView>,
        has_more: bool,
        before: Option<String>,
    },
    ChatDeleted {
        id: String,
        sender: String,
    },
    ChatFailed {
        message: String,
    },
    FileTransfer(FileTransferView),
    RoomsUpdated(Vec<RoomSummary>),
    AudioDevicesUpdated {
        inputs: Vec<AudioDeviceView>,
        outputs: Vec<AudioDeviceView>,
        selected_input: Option<String>,
        selected_output: Option<String>,
    },
    CaptureTargetsUpdated(Vec<CaptureTargetView>),
    MicLevel {
        level_db: i8,
        gate_open: bool,
    },
    ScreenShareStarted,
    ScreenShareStopped,
    ShareFailed {
        message: String,
    },
    RemoteScreenShareStarted {
        identity: String,
    },
    RemoteScreenShareStopped {
        identity: String,
    },
    WatchingStarted {
        identity: String,
        feed: FeedHandle,
    },
    WatchingStopped,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub token_server_url: String,
    pub client_id: String,
    pub display_name: String,
    pub invite_code: String,
}

pub trait SessionPort {
    fn configure(&mut self, _credentials: Credentials) {}

    fn send(&self, command: UiCommand);
    fn try_recv(&mut self) -> Option<UiEvent>;
}
