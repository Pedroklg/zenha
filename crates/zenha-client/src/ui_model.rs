use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use zenha_core::{AdminAction, RemovalReason, RoomKey};
use zenha_protocol::{AdminRoom, RoomSummary};

use crate::platform::settings::NoiseMode;

#[derive(Clone, PartialEq, Eq)]
pub enum UiCommand {
    Join {
        room: String,
    },
    UnlockRoom {
        room: String,
        password: String,
    },
    Leave,
    SetMuted(bool),
    SetDeafened(bool),
    SetDisplayName {
        name: String,
    },
    SetAvatar {
        path: PathBuf,
    },
    ClearAvatar,
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
    CancelCaptureTargets,
    StartScreenShare {
        target_id: String,
        preset: SharePresetView,
    },
    StopScreenShare,
    WatchScreenShare {
        identity: String,
    },
    StopWatching,
    SetUiVisible(bool),
    PlayCue(zenha_core::Cue),
    AdminLogin {
        password: String,
    },
    AdminLogout,
    Admin(AdminAction),
}

impl fmt::Debug for UiCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Join { room } => f.debug_struct("Join").field("room", room).finish(),
            Self::UnlockRoom { room, .. } => f
                .debug_struct("UnlockRoom")
                .field("room", room)
                .field("password", &"<redacted>")
                .finish(),
            Self::Leave => f.write_str("Leave"),
            Self::SetMuted(muted) => f.debug_tuple("SetMuted").field(muted).finish(),
            Self::SetDeafened(deafened) => f.debug_tuple("SetDeafened").field(deafened).finish(),
            Self::SetDisplayName { name } => f
                .debug_struct("SetDisplayName")
                .field("name", name)
                .finish(),
            Self::SetAvatar { path } => f.debug_struct("SetAvatar").field("path", path).finish(),
            Self::ClearAvatar => f.write_str("ClearAvatar"),
            Self::SendChat { text, save } => f
                .debug_struct("SendChat")
                .field("text", text)
                .field("save", save)
                .finish(),
            Self::LoadChatHistory { before } => f
                .debug_struct("LoadChatHistory")
                .field("before", before)
                .finish(),
            Self::DeleteChat { id } => f.debug_struct("DeleteChat").field("id", id).finish(),
            Self::SendFile { path } => f.debug_struct("SendFile").field("path", path).finish(),
            Self::RefreshRooms => f.write_str("RefreshRooms"),
            Self::RefreshAudioDevices => f.write_str("RefreshAudioDevices"),
            Self::SetInputDevice(id) => f.debug_tuple("SetInputDevice").field(id).finish(),
            Self::SetOutputDevice(id) => f.debug_tuple("SetOutputDevice").field(id).finish(),
            Self::SetNoiseSuppression(mode) => {
                f.debug_tuple("SetNoiseSuppression").field(mode).finish()
            }
            Self::SetStreamVolume(percent) => {
                f.debug_tuple("SetStreamVolume").field(percent).finish()
            }
            Self::SetParticipantVolume { identity, percent } => f
                .debug_struct("SetParticipantVolume")
                .field("identity", identity)
                .field("percent", percent)
                .finish(),
            Self::SetNoiseGate(threshold) => {
                f.debug_tuple("SetNoiseGate").field(threshold).finish()
            }
            Self::RefreshCaptureTargets => f.write_str("RefreshCaptureTargets"),
            Self::CancelCaptureTargets => f.write_str("CancelCaptureTargets"),
            Self::StartScreenShare { target_id, preset } => f
                .debug_struct("StartScreenShare")
                .field("target_id", target_id)
                .field("preset", preset)
                .finish(),
            Self::StopScreenShare => f.write_str("StopScreenShare"),
            Self::WatchScreenShare { identity } => f
                .debug_struct("WatchScreenShare")
                .field("identity", identity)
                .finish(),
            Self::StopWatching => f.write_str("StopWatching"),
            Self::SetUiVisible(visible) => f.debug_tuple("SetUiVisible").field(visible).finish(),
            Self::PlayCue(cue) => f.debug_tuple("PlayCue").field(cue).finish(),
            Self::AdminLogin { .. } => f
                .debug_struct("AdminLogin")
                .field("password", &"<redacted>")
                .finish(),
            Self::AdminLogout => f.write_str("AdminLogout"),
            Self::Admin(action) => f.debug_tuple("Admin").field(action).finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharePresetView {
    P720F30,
    P1080F30,
    #[default]
    P1080F60,
}

impl SharePresetView {
    pub const ALL: [SharePresetView; 3] = [Self::P720F30, Self::P1080F30, Self::P1080F60];

    pub fn label(self) -> &'static str {
        match self {
            Self::P720F30 => "720p, 30 fps",
            Self::P1080F30 => "1080p, 30 fps",
            Self::P1080F60 => "1080p, 60 fps",
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
    pub avatar_version: Option<String>,
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
    /// Foto de perfil de alguém já gravada no cache em disco.
    AvatarCached {
        identity: String,
        version: String,
        path: PathBuf,
    },
    /// Versão da foto do usuário local depois de enviar ou remover; `None` = sem foto.
    LocalAvatarChanged {
        version: Option<String>,
    },
    AvatarFailed {
        message: String,
    },
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
    /// Senha aceita; `room` é a sala dona da senha (a pedida ou a sala-mãe dela).
    RoomUnlocked {
        room: String,
        key: RoomKey,
    },
    RoomUnlockFailed {
        message: String,
    },
    /// O servidor recusou a entrada sem chave válida para a sala.
    RoomLocked {
        room: String,
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
    CaptureThumbnail {
        id: String,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    MicLevel {
        level_db: i8,
        gate_open: bool,
    },
    ScreenShareStarted {
        preview: FeedHandle,
    },
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
    AdminChanged {
        active: bool,
    },
    AdminRoomCreated(AdminRoom),
    AdminRoomUpdated(AdminRoom),
    AdminRoomDeleted {
        id: String,
    },
    AdminInviteRotated,
    AdminStoppedScreenShare,
    MovedByAdmin {
        to_room: String,
    },
    RemovedFromRoom {
        reason: RemovalReason,
    },
    AdminFailed {
        message: String,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub token_server_url: String,
    pub client_id: String,
    pub display_name: String,
    pub invite_code: String,
    pub avatar_version: Option<String>,
    pub avatar_cache_dir: Option<PathBuf>,
    pub room_keys: BTreeMap<String, String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("token_server_url", &self.token_server_url)
            .field("client_id", &self.client_id)
            .field("display_name", &self.display_name)
            .field("invite_code", &"<redacted>")
            .finish()
    }
}

pub trait SessionPort {
    fn configure(&mut self, _credentials: Credentials) {}

    fn send(&self, command: UiCommand);
    fn try_recv(&mut self) -> Option<UiEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_debug_redacts_admin_secrets() {
        let login = UiCommand::AdminLogin {
            password: "admin-secret-password".to_owned(),
        };
        assert!(!format!("{login:?}").contains("admin-secret-password"));

        let invite = UiCommand::Admin(AdminAction::RotateInvite {
            new_code: "new-secret-invite-123".to_owned(),
        });
        assert!(!format!("{invite:?}").contains("new-secret-invite-123"));

        let unlock = UiCommand::UnlockRoom {
            room: "vip".to_owned(),
            password: "senha-da-sala".to_owned(),
        };
        assert!(!format!("{unlock:?}").contains("senha-da-sala"));

        let creds = Credentials {
            token_server_url: "https://voz.exemplo".to_owned(),
            client_id: "c1".to_owned(),
            display_name: "Ana".to_owned(),
            invite_code: "invite-secret-456".to_owned(),
            avatar_version: None,
            avatar_cache_dir: None,
            room_keys: BTreeMap::from([("vip".to_owned(), "room-key-789".to_owned())]),
        };
        let creds_dbg = format!("{creds:?}");
        assert!(!creds_dbg.contains("invite-secret-456"));
        assert!(!creds_dbg.contains("room-key-789"));
        assert!(creds_dbg.contains("https://voz.exemplo"));
        assert!(creds_dbg.contains("<redacted>"));
    }
}
