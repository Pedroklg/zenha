//! Estado da interface, ações do usuário e transições de tela do cliente.

mod admin;
mod audio;
mod chat;
mod onboarding;
mod room;
mod room_lock;
mod share;
mod update;

use std::path::PathBuf;
use std::time::Instant;

use zenha_core::{AdminAction, RemovalReason};
use zenha_protocol::RoomSummary;

pub use admin::{AdminDialog, AdminState, parent_options};
pub use audio::{
    AudioState, DEFAULT_DEVICE_LABEL, DEFAULT_PARTICIPANT_VOLUME, FULL_VOLUME, HotkeyState,
    LobbyVoiceState, MAX_PARTICIPANT_VOLUME, MicLevelView, participant_volume_for,
};
pub use chat::{ChatEntry, ChatState, ImageViewer, PendingPastedImage};
pub use onboarding::{CredentialOp, LoginForm, OnboardingState, display_name_error};
pub use room::{ConnectionStatus, RoomState, room_display_title};
pub use room_lock::{RoomPasswordDialog, SecretText, sidebar_order};
pub use share::{LocalShare, SharePicker, ShareState, WatchState, aspect_fit};
pub use update::{UpdateBanner, UpdateEffect, UpdatePhase};

use crate::cli::{compile_time_default_server, resolve_initial_server};
use crate::hotkeys::HotkeyAction;
use crate::settings::{NoiseMode, RoomKeys, Settings, VoiceMode};
use crate::ui_model::{SharePresetView, UiCommand, UiEvent};
use admin::upsert_admin_room;
use audio::{saved_volume_commands, update_participant_volume};
use room::apply_room_event;
use update::UpdateState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Onboarding,
    Lobby,
    Connecting { room: String },
    InRoom(Box<RoomState>),
}

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
}

pub struct AppState {
    pub screen: Screen,
    pub form: LoginForm,
    pub name_draft: String,
    pub onboarding: OnboardingState,
    pub lobby_voice: LobbyVoiceState,
    pub lobby_error: Option<String>,
    pub lobby_notice: Option<String>,
    pub muted_on_join: bool,
    pub invite_copied: bool,
    pub rooms: Vec<RoomSummary>,
    pub audio: AudioState,
    pub hotkeys: HotkeyState,
    pub close_to_tray: bool,
    pub room_sounds: bool,
    pub picker: SharePicker,
    pub admin: AdminState,
    pub avatar_version: Option<String>,
    pub avatar_notice: Option<String>,
    pub room_password: Option<RoomPasswordDialog>,
    room_keys: RoomKeys,
    avatar_cache_dir: Option<PathBuf>,
    client_id: String,
    last_room: String,
    saved_credential: Option<(String, String)>,
    active_password: String,
    pending_password_save: Option<(String, String)>,
    pending_credential_ops: Vec<CredentialOp>,
    pending_commands: Vec<UiCommand>,
    pending_paste_deletes: Vec<PathBuf>,
    audio_configured: bool,
    session_mic_sent: bool,
    session_deafened: bool,
    last_refresh: Option<Instant>,
    pending_save: Option<Settings>,
    update: UpdateState,
    pending_update_effects: Vec<UpdateEffect>,
    pending_move_notice: Option<String>,
}

impl AppState {
    pub fn new(settings: &Settings, saved_password: Option<String>) -> Self {
        Self::new_with_build_default(settings, saved_password, compile_time_default_server())
    }

    pub fn new_with_build_default(
        settings: &Settings,
        saved_password: Option<String>,
        build_default_server: Option<&str>,
    ) -> Self {
        let mut form = LoginForm::from_settings(settings);
        form.server_url = resolve_initial_server(&form.server_url, build_default_server);
        let server = form.server_url.trim().to_owned();
        let saved_password = saved_password.filter(|p| !p.is_empty() && !server.is_empty());
        let open_lobby = form.is_valid() && saved_password.is_some();
        let active_password = saved_password.clone().unwrap_or_default();
        let saved_credential = saved_password.map(|pwd| (server, pwd));
        let audio = AudioState::from_settings(settings);
        let pending_commands = if open_lobby {
            saved_volume_commands(&audio.participant_volumes)
        } else {
            Vec::new()
        };
        Self {
            screen: if open_lobby {
                Screen::Lobby
            } else {
                Screen::Onboarding
            },
            form,
            name_draft: settings.display_name.clone(),
            onboarding: OnboardingState::default(),
            lobby_voice: LobbyVoiceState::from_muted(settings.muted_on_join),
            lobby_error: None,
            lobby_notice: None,
            muted_on_join: settings.muted_on_join,
            invite_copied: false,
            rooms: Vec::new(),
            audio,
            hotkeys: HotkeyState::from_settings(settings),
            close_to_tray: settings.close_to_tray,
            room_sounds: settings.room_sounds,
            picker: SharePicker::default(),
            admin: AdminState::default(),
            avatar_version: settings.avatar_version.clone(),
            avatar_notice: None,
            room_password: None,
            room_keys: settings.room_keys.clone(),
            avatar_cache_dir: None,
            client_id: settings.client_id.clone(),
            last_room: settings.last_room.clone(),
            saved_credential,
            active_password,
            pending_password_save: None,
            pending_credential_ops: Vec::new(),
            pending_commands,
            pending_paste_deletes: Vec::new(),
            audio_configured: false,
            session_mic_sent: false,
            session_deafened: false,
            last_refresh: None,
            pending_save: None,
            update: UpdateState::with_dismissed(settings.dismissed_update_version.clone()),
            pending_update_effects: Vec::new(),
            pending_move_notice: None,
        }
    }

    pub fn set_avatar_cache_dir(&mut self, dir: Option<PathBuf>) {
        self.avatar_cache_dir = dir;
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn tray_muted(&self) -> Option<bool> {
        match &self.screen {
            Screen::InRoom(room) => Some(room.muted),
            _ => None,
        }
    }

    pub fn take_credential_ops(&mut self) -> Vec<CredentialOp> {
        std::mem::take(&mut self.pending_credential_ops)
    }

    pub fn take_pending_commands(&mut self) -> Vec<UiCommand> {
        std::mem::take(&mut self.pending_commands)
    }

    pub fn take_pending_paste_deletes(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pending_paste_deletes)
    }

    pub fn take_pending_save(&mut self) -> Option<Settings> {
        self.pending_save.take()
    }

    fn settings_snapshot(&self) -> Settings {
        Settings {
            client_id: self.client_id.clone(),
            last_room: self.last_room.clone(),
            muted_on_join: self.muted_on_join,
            input_device: self.audio.input.clone(),
            output_device: self.audio.output.clone(),
            noise_suppression: self.audio.noise,
            voice_mode: self.audio.voice_mode,
            stream_volume: (self.audio.stream_volume != FULL_VOLUME)
                .then_some(self.audio.stream_volume),
            participant_volumes: self.audio.participant_volumes.clone(),
            noise_gate_db: self.audio.noise_gate,
            mute_hotkey: self.hotkeys.mute.clone(),
            deafen_hotkey: self.hotkeys.deafen.clone(),
            ptt_hotkey: self.hotkeys.ptt.clone(),
            close_to_tray: self.close_to_tray,
            room_sounds: self.room_sounds,
            dismissed_update_version: self.update.dismissed_version.clone(),
            avatar_version: self.avatar_version.clone(),
            room_keys: self.room_keys.clone(),
            ..self.form.to_settings()
        }
    }

    pub fn handle_action(&mut self, action: UserAction) -> Vec<UiCommand> {
        match action {
            UserAction::SubmitOnboarding => self.submit_onboarding(),
            UserAction::ToggleInviteLink => {
                self.onboarding.link_open = !self.onboarding.link_open;
                self.onboarding.link_error = None;
                Vec::new()
            }
            UserAction::PasteInviteLink(text) => {
                self.paste_invite_link(text);
                Vec::new()
            }
            UserAction::JoinRoom(room) => self.join_room(room),
            UserAction::SubmitRoomPassword => self.submit_room_password(),
            UserAction::CancelRoomPassword => {
                self.room_password = None;
                Vec::new()
            }
            UserAction::CancelConnect => self.cancel_connect(),
            UserAction::ChangeServer => self.change_server(),
            UserAction::SaveDisplayName => self.save_display_name(),
            UserAction::PickAvatar => Vec::new(),
            UserAction::SetAvatar(path) => {
                self.avatar_notice = None;
                vec![UiCommand::SetAvatar { path }]
            }
            UserAction::RemoveAvatar => {
                self.avatar_notice = None;
                if self.avatar_version.is_some() {
                    vec![UiCommand::ClearAvatar]
                } else {
                    Vec::new()
                }
            }
            UserAction::CopyInviteLink => {
                self.invite_copied = true;
                Vec::new()
            }
            UserAction::Leave => self.leave(),
            UserAction::SwitchRoom(room) => self.switch_room(room),
            UserAction::ToggleMute => self.toggle_mute(),
            UserAction::ToggleDeafen => self.toggle_deafen(),
            UserAction::SendChat => self.with_room(RoomState::send_chat),
            UserAction::LoadOlderChat => self.with_room(RoomState::load_older_chat),
            UserAction::DeleteChat(id) => {
                let is_admin = self.admin.active;
                self.with_room(|room| room.delete_chat(id, is_admin))
            }
            UserAction::AttachFile(path) => self.with_room(|_| vec![UiCommand::SendFile { path }]),
            UserAction::StagePastedImage(image) => {
                if let Screen::InRoom(room) = &mut self.screen {
                    if let Some(previous) = room.chat.pending_paste.replace(image)
                        && room
                            .chat
                            .pending_paste
                            .as_ref()
                            .is_some_and(|current| current.path != previous.path)
                    {
                        self.pending_paste_deletes.push(previous.path);
                    }
                } else {
                    self.pending_paste_deletes.push(image.path);
                }
                Vec::new()
            }
            UserAction::SendPastedImage => self.with_room(|room| {
                room.chat
                    .pending_paste
                    .take()
                    .map_or_else(Vec::new, |pending| {
                        vec![UiCommand::SendFile { path: pending.path }]
                    })
            }),
            UserAction::CancelPastedImage => {
                self.discard_pending_paste();
                Vec::new()
            }
            UserAction::PickFile
            | UserAction::SaveReceivedFile { .. }
            | UserAction::OpenImageExternally(_) => Vec::new(),
            UserAction::OpenImageViewer(id) => self.with_room(|room| {
                room.chat.open_image(&id);
                Vec::new()
            }),
            UserAction::CloseImageViewer => self.with_room(|room| {
                room.chat.close_image();
                Vec::new()
            }),
            UserAction::DismissChatNotice => self.with_room(|room| {
                room.chat.notice = None;
                Vec::new()
            }),
            UserAction::OpenSettings => {
                self.audio.open = true;
                self.name_draft.clone_from(&self.form.display_name);
                self.invite_copied = false;
                self.admin.notice = None;
                vec![UiCommand::RefreshAudioDevices]
            }
            UserAction::CloseSettings => {
                self.audio.open = false;
                self.hotkeys.capturing = None;
                self.hotkeys.capture_error = None;
                self.invite_copied = false;
                self.admin.error = None;
                self.admin.notice = None;
                Vec::new()
            }
            UserAction::StartHotkeyCapture(action) => {
                self.hotkeys.capturing = Some(action);
                self.hotkeys.capture_error = None;
                Vec::new()
            }
            UserAction::CancelHotkeyCapture => {
                self.hotkeys.capturing = None;
                self.hotkeys.capture_error = None;
                Vec::new()
            }
            UserAction::RejectHotkey(message) => {
                self.hotkeys.capture_error = Some(message);
                Vec::new()
            }
            UserAction::SetHotkey(action, binding) => {
                self.set_hotkey(action, binding);
                Vec::new()
            }
            UserAction::HotkeyPressed(action) => {
                if self.hotkeys.capturing.is_some() {
                    return Vec::new();
                }
                match action {
                    HotkeyAction::ToggleMute => self.toggle_mute(),
                    HotkeyAction::ToggleDeafen => self.toggle_deafen(),
                    HotkeyAction::PushToTalk => Vec::new(),
                }
            }
            UserAction::RefreshAudioDevices => vec![UiCommand::RefreshAudioDevices],
            UserAction::SelectInputDevice(id) => self.select_input(id),
            UserAction::SelectOutputDevice(id) => self.select_output(id),
            UserAction::SelectNoiseMode(mode) => self.select_noise(mode),
            UserAction::SelectVoiceMode(mode) => self.select_voice_mode(mode),
            UserAction::SetStreamVolume(percent) => {
                let percent = percent.min(FULL_VOLUME);
                if self.audio.stream_volume == percent {
                    return Vec::new();
                }
                self.audio.stream_volume = percent;
                vec![UiCommand::SetStreamVolume(percent)]
            }
            UserAction::SetParticipantVolume { identity, percent } => {
                let Some(applied) = update_participant_volume(
                    &mut self.audio.participant_volumes,
                    identity.clone(),
                    percent,
                ) else {
                    return Vec::new();
                };
                vec![UiCommand::SetParticipantVolume {
                    identity,
                    percent: applied,
                }]
            }
            UserAction::SetNoiseGate(threshold) => {
                if self.audio.noise_gate == threshold {
                    return Vec::new();
                }
                self.audio.noise_gate = threshold;
                vec![UiCommand::SetNoiseGate(threshold)]
            }
            UserAction::SaveAudioSettings => {
                self.pending_save = Some(self.settings_snapshot());
                Vec::new()
            }
            UserAction::SetCloseToTray(enabled) => {
                self.close_to_tray = enabled;
                self.pending_save = Some(self.settings_snapshot());
                Vec::new()
            }
            UserAction::SetRoomSounds(enabled) => {
                self.room_sounds = enabled;
                self.pending_save = Some(self.settings_snapshot());
                Vec::new()
            }
            UserAction::SetMutedOnJoin(enabled) => {
                self.muted_on_join = enabled;
                if matches!(self.screen, Screen::Lobby) && enabled && !self.lobby_voice.deafened {
                    self.lobby_voice.muted = true;
                }
                self.pending_save = Some(self.settings_snapshot());
                Vec::new()
            }
            UserAction::OpenSharePicker => self.open_picker(),
            UserAction::CloseSharePicker => {
                self.picker.close();
                vec![UiCommand::CancelCaptureTargets]
            }
            UserAction::RefreshCaptureTargets => {
                self.picker.reset_loading();
                vec![UiCommand::RefreshCaptureTargets]
            }
            UserAction::SelectCaptureTarget(id) => {
                if self.picker.targets.iter().any(|t| t.id == id) {
                    self.picker.selected = Some(id);
                }
                Vec::new()
            }
            UserAction::SelectSharePreset(preset) => {
                self.picker.preset = preset;
                Vec::new()
            }
            UserAction::StartShare => self.start_share(),
            UserAction::StopShare => self.with_room(|room| {
                if room.share.local == LocalShare::Active {
                    vec![UiCommand::StopScreenShare]
                } else {
                    Vec::new()
                }
            }),
            UserAction::ToggleSharePreviewCollapsed => {
                self.with_room(|room| {
                    room.share.toggle_preview_collapsed();
                    Vec::new()
                });
                Vec::new()
            }
            UserAction::DismissShareError => {
                self.with_room(|room| {
                    room.share.error = None;
                    Vec::new()
                });
                Vec::new()
            }
            UserAction::Watch(identity) => self.with_room(|room| room.share.watch(identity)),
            UserAction::StopWatching => self.with_room(|room| room.share.stop_watching()),
            UserAction::ToggleFullscreen => {
                self.with_room(|room| {
                    room.share.toggle_fullscreen();
                    Vec::new()
                });
                Vec::new()
            }
            UserAction::ToggleChat => {
                if let Screen::InRoom(state) = &mut self.screen {
                    state.chat.open = !state.chat.open;
                    if state.chat.open {
                        state.chat.unread = 0;
                    }
                }
                Vec::new()
            }
            UserAction::StartUpdate => {
                self.start_update();
                Vec::new()
            }
            UserAction::ConfirmUpdate => {
                self.confirm_update();
                Vec::new()
            }
            UserAction::CancelUpdateConfirm => {
                self.cancel_update_confirm();
                Vec::new()
            }
            UserAction::CancelUpdateDownload => {
                self.cancel_update_download();
                Vec::new()
            }
            UserAction::RetryUpdate => {
                self.retry_update();
                Vec::new()
            }
            UserAction::DismissUpdate => {
                self.dismiss_update();
                Vec::new()
            }
            UserAction::DismissLobbyNotice => {
                self.lobby_notice = None;
                Vec::new()
            }
            UserAction::AdminLogin => self.admin_login(),
            UserAction::AdminLogout => self.admin_logout(),
            UserAction::RotateInvite => self.rotate_invite(),
            UserAction::DismissAdminNotice => {
                self.admin.error = None;
                self.admin.notice = None;
                Vec::new()
            }
            UserAction::OpenCreateRoom => {
                if self.admin.active {
                    self.admin.dialog = Some(AdminDialog::CreateRoom {
                        title: String::new(),
                        password: SecretText::default(),
                        parent: None,
                        error: None,
                    });
                }
                Vec::new()
            }
            UserAction::OpenEditRoom(id) => {
                if self.admin.active {
                    self.open_edit_room(id);
                }
                Vec::new()
            }
            UserAction::OpenDeleteRoom(id) => {
                if self.admin.active && self.rooms.iter().any(|r| r.name == id) {
                    let title = room_display_title(&self.rooms, &id).to_owned();
                    self.admin.dialog = Some(AdminDialog::DeleteRoom { id, title });
                }
                Vec::new()
            }
            UserAction::MoveRoomUp(id) => self.move_room(&id, -1),
            UserAction::MoveRoomDown(id) => self.move_room(&id, 1),
            UserAction::ConfirmAdminDialog => self.confirm_admin_dialog(),
            UserAction::CancelAdminDialog => {
                self.admin.dialog = None;
                Vec::new()
            }
            UserAction::AdminMuteParticipant { identity } => {
                let Some((room, _)) = self.remote_target_room(&identity) else {
                    return Vec::new();
                };
                vec![UiCommand::Admin(AdminAction::MuteParticipant {
                    room,
                    identity,
                })]
            }
            UserAction::AdminStopShareParticipant { identity } => {
                let Some((room, _)) = self.remote_target_room(&identity) else {
                    return Vec::new();
                };
                let is_sharing = matches!(
                    &self.screen,
                    Screen::InRoom(state) if state.share.is_sharing_remote(&identity)
                );
                if is_sharing {
                    vec![UiCommand::Admin(AdminAction::StopShareParticipant {
                        room,
                        identity,
                    })]
                } else {
                    Vec::new()
                }
            }
            UserAction::AdminMoveParticipant { identity, to_room } => {
                let Some((room, _)) = self.remote_target_room(&identity) else {
                    return Vec::new();
                };
                if to_room.trim().is_empty() || to_room == room {
                    return Vec::new();
                }
                vec![UiCommand::Admin(AdminAction::MoveParticipant {
                    room,
                    identity,
                    to_room,
                })]
            }
            UserAction::OpenKickParticipant { identity } => {
                let Some((room, name)) = self.remote_target_room(&identity) else {
                    return Vec::new();
                };
                self.admin.dialog = Some(AdminDialog::KickParticipant {
                    room,
                    identity,
                    name,
                });
                Vec::new()
            }
        }
    }

    pub fn apply_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::RoomsUpdated(mut rooms) => {
                if !matches!(self.screen, Screen::Onboarding) {
                    for room in &mut rooms {
                        room.participants
                            .sort_by_cached_key(|name| name.to_lowercase());
                    }
                    self.rooms = rooms;
                    self.lobby_error = None;
                    self.prune_admin_dialog_for_rooms();
                    if let Some((server, password)) = self.pending_password_save.take() {
                        self.saved_credential = Some((server.clone(), password.clone()));
                        self.pending_credential_ops
                            .push(CredentialOp::Store { server, password });
                    }
                }
            }
            UiEvent::RoomsFailed { message } => {
                if self.room_password.as_ref().is_some_and(|d| d.busy) {
                    self.on_room_unlock_failed(message);
                } else if matches!(self.screen, Screen::Lobby | Screen::Connecting { .. }) {
                    self.lobby_error = Some(message);
                }
            }
            UiEvent::RoomUnlocked { room, key } => {
                let commands = self.on_room_unlocked(&room, &key.0);
                self.pending_commands.extend(commands);
            }
            UiEvent::RoomUnlockFailed { message } => self.on_room_unlock_failed(message),
            UiEvent::RoomLocked { room } => self.on_room_locked(room),
            UiEvent::InviteRejected => self.on_invite_rejected(),
            UiEvent::AudioDevicesUpdated {
                inputs, outputs, ..
            } => {
                self.audio.inputs = inputs;
                self.audio.outputs = outputs;
                self.audio.loaded = true;
            }
            UiEvent::Joined {
                room,
                local_identity,
                participants,
            } => self.on_joined(room, local_identity, participants),
            UiEvent::LocalAvatarChanged { version } => {
                self.avatar_notice = None;
                if self.avatar_version != version {
                    self.avatar_version = version;
                    self.pending_save = Some(self.settings_snapshot());
                }
            }
            UiEvent::AvatarFailed { message } => self.avatar_notice = Some(message),
            UiEvent::AvatarCached { .. } => {}
            UiEvent::CaptureTargetsUpdated(targets) => self.picker.set_targets(targets),
            UiEvent::CaptureThumbnail {
                id,
                width,
                height,
                rgba,
            } => self.picker.set_thumbnail(id, width, height, rgba),
            UiEvent::Failed { message } => {
                self.pending_move_notice = None;
                if matches!(self.screen, Screen::Connecting { .. } | Screen::InRoom(_)) {
                    self.lobby_error = Some(message);
                }
                self.leave_to_lobby();
            }
            UiEvent::MicLevel {
                level_db,
                gate_open,
            } => {
                if matches!(self.screen, Screen::InRoom(_)) && self.audio.noise.uses_own_capture() {
                    self.audio.mic_level = Some(MicLevelView {
                        level_db,
                        gate_open,
                    });
                }
            }
            UiEvent::Left => {
                self.pending_move_notice = None;
                self.leave_to_lobby();
            }
            UiEvent::AdminChanged { active } => {
                if active {
                    self.admin.active = true;
                    self.admin.logging_in = false;
                    self.admin.error = None;
                } else {
                    self.admin.reset();
                }
            }
            UiEvent::AdminRoomCreated(room) | UiEvent::AdminRoomUpdated(room) => {
                if !matches!(self.screen, Screen::Onboarding) {
                    upsert_admin_room(&mut self.rooms, room);
                }
            }
            UiEvent::AdminRoomDeleted { id } => {
                if !matches!(self.screen, Screen::Onboarding) {
                    self.rooms.retain(|r| r.name != id);
                    self.prune_admin_dialog_for_rooms();
                }
            }
            UiEvent::AdminInviteRotated => {
                let code = std::mem::take(&mut self.admin.new_invite_input);
                if !code.is_empty() {
                    self.active_password = code.clone();
                    if !self.form.invite_code.is_empty() {
                        self.form.invite_code = code.clone();
                    }
                    if let Some((server, _)) = self.saved_credential.take() {
                        self.saved_credential = Some((server.clone(), code.clone()));
                        self.pending_credential_ops.push(CredentialOp::Store {
                            server,
                            password: code,
                        });
                    }
                }
                self.admin
                    .set_notice("Código de convite atualizado.".to_owned());
            }
            UiEvent::AdminStoppedScreenShare => {
                self.set_chat_notice("Um admin parou sua transmissão".to_owned());
            }
            UiEvent::MovedByAdmin { to_room } => {
                let title = room_display_title(&self.rooms, &to_room).to_owned();
                let message = format!("Um admin moveu você para {title}");
                self.pending_move_notice = Some(message.clone());
                self.set_chat_notice(message);
                if matches!(
                    &self.admin.dialog,
                    Some(AdminDialog::KickParticipant { .. })
                ) {
                    self.admin.dialog = None;
                }
            }
            UiEvent::RemovedFromRoom { reason } => {
                self.pending_move_notice = None;
                if !matches!(self.screen, Screen::Onboarding | Screen::Lobby) {
                    self.leave_to_lobby();
                }
                if !matches!(self.screen, Screen::Onboarding) {
                    let message = match reason {
                        RemovalReason::Kicked => "Você foi removido da sala".to_owned(),
                        RemovalReason::RoomDeleted => "A sala foi apagada".to_owned(),
                    };
                    self.lobby_notice = Some(message);
                }
            }
            UiEvent::AdminFailed { message } => {
                self.admin.logging_in = false;
                self.admin.set_error(message.clone());
                if !self.audio.open {
                    match &mut self.screen {
                        Screen::InRoom(room) => room.chat.set_notice(message),
                        Screen::Lobby | Screen::Connecting { .. } => {
                            self.lobby_notice = Some(message);
                        }
                        Screen::Onboarding => {}
                    }
                }
            }
            other => {
                if let UiEvent::ParticipantLeft { ref identity } = other
                    && matches!(
                        &self.admin.dialog,
                        Some(AdminDialog::KickParticipant { identity: target, .. })
                            if target == identity
                    )
                {
                    self.admin.dialog = None;
                }
                let cue = if let Screen::InRoom(state) = &mut self.screen {
                    apply_room_event(state, other)
                } else {
                    None
                };
                if let Some(cmd) = cue.and_then(|cue| self.room_cue(cue)) {
                    self.pending_commands.push(cmd);
                }
            }
        }
    }
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::{person, state_with_code, valid_settings};

    #[test]
    fn avatar_actions_events_and_credentials_follow_the_local_photo() {
        let mut state = AppState::new(&valid_settings(), None);
        assert!(state.credentials().avatar_version.is_none());
        assert!(state.handle_action(UserAction::RemoveAvatar).is_empty());
        let path = PathBuf::from("foto.png");
        assert_eq!(
            state.handle_action(UserAction::SetAvatar(path.clone())),
            vec![UiCommand::SetAvatar { path }]
        );

        state.apply_event(UiEvent::LocalAvatarChanged {
            version: Some("0123456789abcdef".to_owned()),
        });
        assert_eq!(state.avatar_version.as_deref(), Some("0123456789abcdef"));
        assert_eq!(
            state.take_pending_save().unwrap().avatar_version.as_deref(),
            Some("0123456789abcdef")
        );
        assert_eq!(
            state.credentials().avatar_version.as_deref(),
            Some("0123456789abcdef")
        );
        assert_eq!(
            state.handle_action(UserAction::RemoveAvatar),
            vec![UiCommand::ClearAvatar]
        );

        state.apply_event(UiEvent::AvatarFailed {
            message: "falhou".to_owned(),
        });
        assert_eq!(state.avatar_notice.as_deref(), Some("falhou"));
        state.apply_event(UiEvent::LocalAvatarChanged { version: None });
        assert!(state.avatar_notice.is_none());
        assert!(state.take_pending_save().unwrap().avatar_version.is_none());
    }

    #[test]
    fn close_to_tray_loads_from_settings_and_saves_on_change() {
        let mut state = AppState::new(&valid_settings(), None);
        assert!(state.close_to_tray);
        assert!(
            state
                .handle_action(UserAction::SetCloseToTray(false))
                .is_empty()
        );
        assert!(!state.close_to_tray);
        let saved = state.take_pending_save().unwrap();
        assert!(!saved.close_to_tray);

        let restored = AppState::new(&saved, None);
        assert!(!restored.close_to_tray);
    }

    #[test]
    fn tray_muted_reflects_room_presence_and_mute_state() {
        let mut state = state_with_code();
        assert_eq!(state.tray_muted(), None);

        state.handle_action(UserAction::SubmitOnboarding);
        assert_eq!(state.tray_muted(), None);

        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert_eq!(state.tray_muted(), None);

        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("ana")],
        });
        assert_eq!(state.tray_muted(), Some(false));

        state.handle_action(UserAction::ToggleMute);
        assert_eq!(state.tray_muted(), Some(true));

        state.handle_action(UserAction::Leave);
        assert_eq!(state.tray_muted(), None);
    }
}
