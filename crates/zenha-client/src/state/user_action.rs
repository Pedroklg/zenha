//! Actions the user triggers in the interface.

use std::path::PathBuf;

use crate::platform::hotkeys::HotkeyAction;
use crate::platform::settings::{NoiseMode, VoiceMode};
use crate::ui_model::SharePresetView;

use super::PendingPastedImage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserAction {
    SubmitOnboarding,
    ToggleInviteLink,
    PasteInviteLink(String),
    JoinRoom(String),
    SubmitRoomPassword,
    CancelRoomPassword,
    CancelConnect,
    ChangeServer,
    SaveDisplayName,
    /// Effect handled by the app (file dialog); the state does not change.
    PickAvatar,
    SetAvatar(PathBuf),
    RemoveAvatar,
    CopyInviteLink,
    ToggleMute,
    ToggleDeafen,
    Leave,
    SwitchRoom(String),
    SendChat,
    LoadOlderChat,
    DeleteChat(String),
    AttachFile(PathBuf),
    StagePastedImage(PendingPastedImage),
    SendPastedImage,
    CancelPastedImage,
    /// Effect handled by the app (file dialog); the state does not change.
    PickFile,
    /// Effect handled by the app (save dialog and copy); the state does not change.
    SaveReceivedFile {
        source: PathBuf,
        name: String,
    },
    OpenImageViewer(String),
    CloseImageViewer,
    /// Effect handled by the app (spawns OS file viewer); the state does not change.
    OpenImageExternally(PathBuf),
    DismissChatNotice,
    ToggleChat,
    OpenSettings,
    CloseSettings,
    RefreshAudioDevices,
    SelectInputDevice(Option<String>),
    SelectOutputDevice(Option<String>),
    SelectNoiseMode(NoiseMode),
    SetEchoCancellation(bool),
    SelectVoiceMode(VoiceMode),
    /// Applies the volume while the slider moves; `SaveAudioSettings` stores it once it stops.
    SetStreamVolume(u8),
    /// Applies a remote participant's volume while adjusting; `SaveAudioSettings` persists it.
    SetParticipantVolume {
        identity: String,
        percent: u16,
    },
    /// Applies the noise gate threshold at once; `SaveAudioSettings` stores it.
    SetNoiseGate(Option<i8>),
    SaveAudioSettings,
    StartHotkeyCapture(HotkeyAction),
    CancelHotkeyCapture,
    RejectHotkey(String),
    SetHotkey(HotkeyAction, Option<String>),
    HotkeyPressed(HotkeyAction),
    SetCloseToTray(bool),
    SetRoomSounds(bool),
    SetMutedOnJoin(bool),
    OpenSharePicker,
    CloseSharePicker,
    RefreshCaptureTargets,
    SelectCaptureTarget(String),
    SelectSharePreset(SharePresetView),
    StartShare,
    StopShare,
    ToggleSharePreviewCollapsed,
    DismissShareError,
    Watch(String),
    StopWatching,
    ToggleFullscreen,
    StartUpdate,
    ConfirmUpdate,
    CancelUpdateConfirm,
    CancelUpdateDownload,
    RetryUpdate,
    DismissUpdate,
    DismissLobbyNotice,
    AdminLogin,
    AdminLogout,
    RotateInvite,
    DismissAdminNotice,
    OpenCreateRoom,
    OpenEditRoom(String),
    OpenDeleteRoom(String),
    MoveRoomUp(String),
    MoveRoomDown(String),
    ConfirmAdminDialog,
    CancelAdminDialog,
    AdminMuteParticipant {
        identity: String,
    },
    AdminStopShareParticipant {
        identity: String,
    },
    AdminMoveParticipant {
        identity: String,
        to_room: String,
    },
    OpenKickParticipant {
        identity: String,
    },
    CreateWebhook,
    OpenRevokeWebhook(String),
    DismissCreatedWebhook,
}
