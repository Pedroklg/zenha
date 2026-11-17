//! Interface state, user actions and client screen transitions.

mod admin;
mod audio;
mod chat;
mod events;
mod handle;
mod onboarding;
mod room;
mod room_lock;
mod share;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod update;
mod user_action;
mod webhooks;

use std::path::PathBuf;
use std::time::Instant;

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
pub use user_action::UserAction;
pub use webhooks::CreatedWebhookNotice;

use crate::cli::{compile_time_default_server, resolve_initial_server};
use crate::platform::settings::{RoomKeys, Settings};
use crate::ui_model::UiCommand;
use audio::saved_volume_commands;
use update::UpdateState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Onboarding,
    Lobby,
    Connecting { room: String },
    InRoom(Box<RoomState>),
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
}
