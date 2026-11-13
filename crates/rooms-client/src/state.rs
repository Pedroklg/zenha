use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rooms_core::Cue;
use rooms_protocol::{
    ChatMessage, MAX_CHAT_MESSAGE_LEN, MAX_DISPLAY_NAME_LEN, MAX_ROOM_NAME_LEN, RoomSummary,
    ValidationError, validate_chat_text, validate_display_name, validate_room_name,
};

use crate::hotkeys::{HotkeyAction, binding_label};
use crate::settings::{NoiseMode, Settings, VoiceMode};
use crate::ui_model::{
    AudioDeviceView, CaptureTargetView, Credentials, FeedHandle, FileTransferView, ParticipantView,
    SharePresetView, TargetKindView, UiCommand, UiEvent,
};

pub const MAX_CHAT_HISTORY: usize = 500;
pub const ROOMS_REFRESH_INTERVAL: Duration = Duration::from_secs(3);
pub const PTT_RELEASE_DELAY: Duration = Duration::from_millis(200);
pub const DEFAULT_DEVICE_LABEL: &str = "Padrão do sistema";
pub const MISSING_DEVICE_LABEL: &str = "(indisponível)";
pub const LOADING_DEVICE_LABEL: &str = "Carregando...";
pub const CHAT_COUNTER_THRESHOLD: usize = MAX_CHAT_MESSAGE_LEN * 8 / 10;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginForm {
    pub display_name: String,
    pub server_url: String,
    pub invite_code: String,
    pub room: String,
    pub muted_on_join: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginErrors {
    pub display_name: Option<String>,
    pub server_url: Option<String>,
    pub invite_code: Option<String>,
    pub room: Option<String>,
}

impl LoginErrors {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.server_url.is_none()
            && self.invite_code.is_none()
            && self.room.is_none()
    }
}

impl LoginForm {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            display_name: settings.display_name.clone(),
            server_url: settings.token_server_url.clone(),
            invite_code: String::new(),
            room: settings.last_room.clone(),
            muted_on_join: settings.muted_on_join,
        }
    }

    pub fn to_settings(&self) -> Settings {
        Settings {
            display_name: self.display_name.trim().to_owned(),
            token_server_url: self.server_url.trim().to_owned(),
            last_room: self.room.trim().to_owned(),
            muted_on_join: self.muted_on_join,
            ..Settings::default()
        }
    }

    pub fn errors(&self) -> LoginErrors {
        LoginErrors {
            display_name: validate_display_name(&self.display_name)
                .err()
                .map(display_name_message),
            server_url: server_url_error(&self.server_url),
            invite_code: self
                .invite_code
                .trim()
                .is_empty()
                .then(|| "Informe o código de convite.".to_owned()),
            room: validate_room_name(self.room.trim())
                .err()
                .map(room_name_message),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.errors().is_empty()
    }
}

fn display_name_message(err: ValidationError) -> String {
    match err {
        ValidationError::Empty => "Informe seu nome.".to_owned(),
        ValidationError::TooLong { .. } => {
            format!("O nome pode ter no máximo {MAX_DISPLAY_NAME_LEN} caracteres.")
        }
        ValidationError::InvalidCharacter => "O nome tem caracteres não permitidos.".to_owned(),
    }
}

fn room_name_message(err: ValidationError) -> String {
    match err {
        ValidationError::Empty => "Informe a sala.".to_owned(),
        ValidationError::TooLong { .. } => {
            format!("O nome da sala pode ter no máximo {MAX_ROOM_NAME_LEN} caracteres.")
        }
        ValidationError::InvalidCharacter => {
            "Use só letras minúsculas sem acento, números, - e _.".to_owned()
        }
    }
}

fn server_url_error(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return Some("Informe o endereço do servidor.".to_owned());
    }
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"));
    match rest {
        Some(rest) if !rest.is_empty() && !url.chars().any(char::is_whitespace) => None,
        _ => Some("O endereço deve começar com http:// ou https:// e não ter espaços.".to_owned()),
    }
}

pub const FULL_VOLUME: u8 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioState {
    pub open: bool,
    pub inputs: Vec<AudioDeviceView>,
    pub outputs: Vec<AudioDeviceView>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub noise: NoiseMode,
    pub voice_mode: VoiceMode,
    pub stream_volume: u8,
    pub noise_gate: Option<i8>,
    pub mic_level: Option<MicLevelView>,
    loaded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicLevelView {
    pub level_db: i8,
    pub gate_open: bool,
}

impl AudioState {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            input: settings.input_device.clone(),
            output: settings.output_device.clone(),
            noise: settings.noise_suppression,
            voice_mode: settings.voice_mode,
            stream_volume: settings
                .stream_volume
                .unwrap_or(FULL_VOLUME)
                .min(FULL_VOLUME),
            noise_gate: settings.noise_gate_db,
            ..Self::default()
        }
    }

    pub fn input_label(&self) -> String {
        selected_label(&self.inputs, self.input.as_deref(), self.loaded)
    }

    pub fn output_label(&self) -> String {
        selected_label(&self.outputs, self.output.as_deref(), self.loaded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HotkeyState {
    pub mute: Option<String>,
    pub deafen: Option<String>,
    pub ptt: Option<String>,
    pub capturing: Option<HotkeyAction>,
    pub capture_error: Option<String>,
    pub failed: Vec<HotkeyAction>,
}

impl HotkeyState {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            mute: settings.mute_hotkey.clone(),
            deafen: settings.deafen_hotkey.clone(),
            ptt: settings.ptt_hotkey.clone(),
            ..Self::default()
        }
    }

    pub fn binding(&self, action: HotkeyAction) -> Option<&str> {
        match action {
            HotkeyAction::ToggleMute => self.mute.as_deref(),
            HotkeyAction::ToggleDeafen => self.deafen.as_deref(),
            HotkeyAction::PushToTalk => self.ptt.as_deref(),
        }
    }

    pub fn registrations(&self, push_to_talk: bool) -> Vec<(HotkeyAction, String)> {
        if self.capturing.is_some() {
            return Vec::new();
        }
        HotkeyAction::ALL
            .into_iter()
            .filter(|&action| action != HotkeyAction::PushToTalk || push_to_talk)
            .filter_map(|action| self.binding(action).map(|b| (action, b.to_owned())))
            .collect()
    }
}

fn selected_label(devices: &[AudioDeviceView], selected: Option<&str>, loaded: bool) -> String {
    let Some(id) = selected else {
        return DEFAULT_DEVICE_LABEL.to_owned();
    };
    match devices.iter().find(|device| device.id == id) {
        Some(device) => device.name.clone(),
        None if loaded => MISSING_DEVICE_LABEL.to_owned(),
        None => LOADING_DEVICE_LABEL.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharePicker {
    pub open: bool,
    pub targets: Vec<CaptureTargetView>,
    pub selected: Option<String>,
    pub preset: SharePresetView,
    pub loaded: bool,
}

impl SharePicker {
    pub fn screens(&self) -> impl Iterator<Item = &CaptureTargetView> {
        self.targets
            .iter()
            .filter(|t| t.kind == TargetKindView::Screen)
    }

    pub fn windows(&self) -> impl Iterator<Item = &CaptureTargetView> {
        self.targets
            .iter()
            .filter(|t| t.kind == TargetKindView::Window)
    }

    pub fn can_start(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|id| self.targets.iter().any(|t| t.id == *id))
    }

    fn set_targets(&mut self, targets: Vec<CaptureTargetView>) {
        self.targets = targets;
        self.loaded = true;
        if !self.can_start() {
            let first = self
                .screens()
                .chain(self.windows())
                .next()
                .map(|t| t.id.clone());
            self.selected = first;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalShare {
    #[default]
    Idle,
    Starting,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchState {
    pub identity: String,
    pub feed: FeedHandle,
    pub fullscreen: bool,
    pub serial: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShareState {
    pub local: LocalShare,
    pub error: Option<String>,
    pub remote: Vec<String>,
    pub watching: Option<WatchState>,
    serial: u64,
}

impl ShareState {
    pub fn is_sharing_remote(&self, identity: &str) -> bool {
        self.remote.iter().any(|id| id == identity)
    }

    pub fn is_watching(&self, identity: &str) -> bool {
        self.watching
            .as_ref()
            .is_some_and(|w| w.identity == identity)
    }

    fn remove_remote(&mut self, identity: &str) {
        self.remote.retain(|id| id != identity);
        if self.is_watching(identity) {
            self.watching = None;
        }
    }

    fn apply(&mut self, event: UiEvent) {
        match event {
            UiEvent::ScreenShareStarted => self.local = LocalShare::Active,
            UiEvent::ScreenShareStopped => self.local = LocalShare::Idle,
            UiEvent::ShareFailed { message } => {
                self.error = Some(message);
                if self.local == LocalShare::Starting {
                    self.local = LocalShare::Idle;
                }
            }
            UiEvent::RemoteScreenShareStarted { identity } => {
                if !self.is_sharing_remote(&identity) {
                    self.remote.push(identity);
                }
            }
            UiEvent::RemoteScreenShareStopped { identity } => self.remove_remote(&identity),
            UiEvent::WatchingStarted { identity, feed } => {
                self.serial += 1;
                self.watching = Some(WatchState {
                    identity,
                    feed,
                    fullscreen: false,
                    serial: self.serial,
                });
            }
            UiEvent::WatchingStopped => self.watching = None,
            UiEvent::ParticipantLeft { identity } => self.remove_remote(&identity),
            _ => {}
        }
    }

    fn start(&mut self) {
        self.error = None;
        self.local = LocalShare::Starting;
    }

    fn watch(&mut self, identity: String) -> Vec<UiCommand> {
        if !self.is_sharing_remote(&identity) || self.is_watching(&identity) {
            return Vec::new();
        }
        vec![UiCommand::WatchScreenShare { identity }]
    }

    fn stop_watching(&mut self) -> Vec<UiCommand> {
        if self.watching.take().is_some() {
            vec![UiCommand::StopWatching]
        } else {
            Vec::new()
        }
    }

    fn toggle_fullscreen(&mut self) {
        if let Some(watching) = &mut self.watching {
            watching.fullscreen = !watching.fullscreen;
        }
    }
}

/// Size that fits `frame` inside `avail` keeping the aspect ratio.
pub fn aspect_fit(avail: (f32, f32), frame: (usize, usize)) -> (f32, f32) {
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    if fw <= 0.0 || fh <= 0.0 || avail.0 <= 0.0 || avail.1 <= 0.0 {
        return (0.0, 0.0);
    }
    let scale = (avail.0 / fw).min(avail.1 / fh);
    (fw * scale, fh * scale)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Reconnecting,
}

impl ConnectionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "conectando...",
            Self::Connected => "conectado",
            Self::Reconnecting => "reconectando...",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    Text(ChatMessage),
    File(FileTransferView),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatState {
    pub messages: VecDeque<ChatEntry>,
    pub unread: usize,
    pub draft: String,
    pub open: bool,
    pub notice: Option<String>,
}

impl ChatState {
    fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            unread: 0,
            draft: String::new(),
            open: true,
            notice: None,
        }
    }

    fn push(&mut self, entry: ChatEntry, counts_as_unread: bool) {
        if self.messages.len() >= MAX_CHAT_HISTORY {
            self.messages.pop_front();
        }
        self.messages.push_back(entry);
        if counts_as_unread && !self.open {
            self.unread += 1;
        }
    }

    fn push_message(&mut self, message: ChatMessage) {
        self.push(ChatEntry::Text(message), true);
    }

    fn apply_transfer(&mut self, view: FileTransferView) {
        let existing = self.messages.iter_mut().find_map(|entry| match entry {
            ChatEntry::File(file) if file.id == view.id => Some(file),
            _ => None,
        });
        match existing {
            Some(file) => file.state = view.state,
            None => {
                let counts = !view.outgoing;
                self.push(ChatEntry::File(view), counts);
            }
        }
    }

    pub fn set_notice(&mut self, text: String) {
        self.notice = Some(text);
    }

    fn clear(&mut self) {
        self.messages.clear();
        self.unread = 0;
        self.draft.clear();
        self.notice = None;
    }

    pub fn draft_len(&self) -> usize {
        self.draft.chars().count()
    }

    pub fn show_counter(&self) -> bool {
        self.draft_len() >= CHAT_COUNTER_THRESHOLD
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomState {
    pub room: String,
    pub participants: Vec<ParticipantView>,
    pub status: ConnectionStatus,
    pub muted: bool,
    pub deafened: bool,
    muted_before_deafen: bool,
    pub ptt_held: bool,
    pub ptt_release_at: Option<Instant>,
    pub mic_sent: bool,
    pub chat: ChatState,
    pub share: ShareState,
}

impl RoomState {
    fn new(room: String, participants: Vec<ParticipantView>, muted: bool, mode: VoiceMode) -> Self {
        let mic_sent = muted || mode == VoiceMode::PushToTalk;
        Self {
            room,
            participants,
            status: ConnectionStatus::Connected,
            muted,
            deafened: false,
            muted_before_deafen: muted,
            ptt_held: false,
            ptt_release_at: None,
            mic_sent,
            chat: ChatState::new(),
            share: ShareState::default(),
        }
    }

    fn effective_muted(&self, mode: VoiceMode) -> bool {
        self.muted
            || (mode == VoiceMode::PushToTalk && !(self.ptt_held || self.ptt_release_at.is_some()))
    }

    fn sync_mic(&mut self, mode: VoiceMode) -> Option<UiCommand> {
        let effective = self.effective_muted(mode);
        (effective != self.mic_sent).then(|| {
            self.mic_sent = effective;
            UiCommand::SetMuted(effective)
        })
    }

    fn upsert_participant(&mut self, participant: ParticipantView) {
        match self
            .participants
            .iter_mut()
            .find(|p| p.identity == participant.identity)
        {
            Some(existing) => *existing = participant,
            None => self.participants.push(participant),
        }
    }

    fn toggle_mute(&mut self, mode: VoiceMode) -> Vec<UiCommand> {
        if self.deafened {
            return Vec::new();
        }
        self.muted = !self.muted;
        let cue = if self.muted { Cue::Muted } else { Cue::Unmuted };
        let mut commands = Vec::new();
        if let Some(cmd) = self.sync_mic(mode) {
            commands.push(cmd);
        }
        commands.push(UiCommand::PlayCue(cue));
        commands
    }

    fn toggle_deafen(&mut self, mode: VoiceMode) -> Vec<UiCommand> {
        if self.deafened {
            self.deafened = false;
            self.muted = self.muted_before_deafen;
            let effective = self.effective_muted(mode);
            self.mic_sent = effective;
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(effective),
                UiCommand::PlayCue(Cue::Undeafened),
            ]
        } else {
            self.muted_before_deafen = self.muted;
            self.deafened = true;
            self.muted = true;
            self.mic_sent = true;
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened),
            ]
        }
    }

    fn send_chat(&mut self) -> Vec<UiCommand> {
        if validate_chat_text(&self.chat.draft).is_err() {
            return Vec::new();
        }
        let text = self.chat.draft.trim().to_owned();
        self.chat.draft.clear();
        vec![UiCommand::SendChat { text }]
    }

    fn reset_for_room(&mut self, room: String) {
        self.room = room;
        self.participants.clear();
        self.chat.clear();
        self.share = ShareState::default();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Login,
    Connecting { room: String },
    InRoom(Box<RoomState>),
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserAction {
    SubmitLogin,
    CancelConnect,
    Retry,
    ToggleMute,
    ToggleDeafen,
    Leave,
    SwitchRoom(String),
    SendChat,
    AttachFile(PathBuf),
    /// Effect handled by the app (file dialog); the state does not change.
    PickFile,
    /// Effect handled by the app (save dialog and copy); the state does not change.
    SaveReceivedFile {
        source: PathBuf,
        name: String,
    },
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
    /// Applies the noise gate threshold at once; `SaveAudioSettings` stores it.
    SetNoiseGate(Option<i8>),
    SaveAudioSettings,
    StartHotkeyCapture(HotkeyAction),
    CancelHotkeyCapture,
    RejectHotkey(String),
    SetHotkey(HotkeyAction, Option<String>),
    HotkeyPressed(HotkeyAction),
    OpenSharePicker,
    CloseSharePicker,
    RefreshCaptureTargets,
    SelectCaptureTarget(String),
    SelectSharePreset(SharePresetView),
    StartShare,
    StopShare,
    DismissShareError,
    Watch(String),
    StopWatching,
    ToggleFullscreen,
}

#[derive(Debug)]
pub struct AppState {
    pub screen: Screen,
    pub form: LoginForm,
    pub rooms: Vec<RoomSummary>,
    pub audio: AudioState,
    pub hotkeys: HotkeyState,
    pub picker: SharePicker,
    last_refresh: Option<Instant>,
    pending_save: Option<Settings>,
}

impl AppState {
    pub fn new(settings: &Settings) -> Self {
        Self {
            screen: Screen::Login,
            form: LoginForm::from_settings(settings),
            rooms: Vec::new(),
            audio: AudioState::from_settings(settings),
            hotkeys: HotkeyState::from_settings(settings),
            picker: SharePicker::default(),
            last_refresh: None,
            pending_save: None,
        }
    }

    pub fn set_hotkey_failures(&mut self, failed: Vec<HotkeyAction>) {
        self.hotkeys.failed = failed;
    }

    pub fn credentials(&self) -> Credentials {
        Credentials {
            token_server_url: self.form.server_url.trim().to_owned(),
            display_name: self.form.display_name.trim().to_owned(),
            invite_code: self.form.invite_code.clone(),
        }
    }

    pub fn can_submit_login(&self) -> bool {
        matches!(self.screen, Screen::Login) && self.form.is_valid()
    }

    fn settings_snapshot(&self) -> Settings {
        Settings {
            input_device: self.audio.input.clone(),
            output_device: self.audio.output.clone(),
            noise_suppression: self.audio.noise,
            voice_mode: self.audio.voice_mode,
            stream_volume: (self.audio.stream_volume != FULL_VOLUME)
                .then_some(self.audio.stream_volume),
            noise_gate_db: self.audio.noise_gate,
            mute_hotkey: self.hotkeys.mute.clone(),
            deafen_hotkey: self.hotkeys.deafen.clone(),
            ptt_hotkey: self.hotkeys.ptt.clone(),
            ..self.form.to_settings()
        }
    }

    pub fn push_to_talk(&mut self, pressed: bool, now: Instant) -> Vec<UiCommand> {
        if self.audio.voice_mode != VoiceMode::PushToTalk {
            return Vec::new();
        }
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        if pressed {
            room.ptt_held = true;
            room.ptt_release_at = None;
            room.sync_mic(self.audio.voice_mode).into_iter().collect()
        } else {
            if room.ptt_held {
                room.ptt_held = false;
                room.ptt_release_at = Some(now + PTT_RELEASE_DELAY);
            }
            Vec::new()
        }
    }

    pub fn poll_push_to_talk(&mut self, now: Instant) -> (Vec<UiCommand>, Option<Duration>) {
        let Screen::InRoom(room) = &mut self.screen else {
            return (Vec::new(), None);
        };
        let Some(deadline) = room.ptt_release_at else {
            return (Vec::new(), None);
        };
        if now >= deadline {
            room.ptt_release_at = None;
            let commands = room.sync_mic(self.audio.voice_mode).into_iter().collect();
            (commands, None)
        } else {
            (Vec::new(), Some(deadline - now))
        }
    }

    pub fn push_to_talk_hint(&self) -> Option<String> {
        if self.audio.voice_mode != VoiceMode::PushToTalk {
            return None;
        }
        if let Screen::InRoom(room) = &self.screen
            && !room.effective_muted(VoiceMode::PushToTalk)
        {
            return Some("Falando (push-to-talk)".to_owned());
        }
        match self.hotkeys.ptt.as_deref() {
            Some(binding) => Some(format!("Segure {} para falar", binding_label(binding))),
            None => Some("Defina a tecla de push-to-talk nas Configurações".to_owned()),
        }
    }

    pub fn take_pending_save(&mut self) -> Option<Settings> {
        self.pending_save.take()
    }

    pub fn poll_refresh(&mut self, now: Instant) -> Option<UiCommand> {
        if !matches!(self.screen, Screen::InRoom(_)) {
            return None;
        }
        let due = self
            .last_refresh
            .is_none_or(|last| now.saturating_duration_since(last) >= ROOMS_REFRESH_INTERVAL);
        if due {
            self.last_refresh = Some(now);
            Some(UiCommand::RefreshRooms)
        } else {
            None
        }
    }

    pub fn handle_action(&mut self, action: UserAction) -> Vec<UiCommand> {
        let voice_mode = self.audio.voice_mode;
        match action {
            UserAction::SubmitLogin => self.submit_login(),
            UserAction::CancelConnect => self.cancel_connect(),
            UserAction::Retry => {
                if matches!(self.screen, Screen::Error { .. }) {
                    self.screen = Screen::Login;
                }
                Vec::new()
            }
            UserAction::Leave => self.leave(),
            UserAction::SwitchRoom(room) => self.switch_room(room),
            UserAction::ToggleMute => self.with_room(|room| room.toggle_mute(voice_mode)),
            UserAction::ToggleDeafen => self.with_room(|room| room.toggle_deafen(voice_mode)),
            UserAction::SendChat => self.with_room(RoomState::send_chat),
            UserAction::AttachFile(path) => self.with_room(|_| vec![UiCommand::SendFile { path }]),
            UserAction::PickFile | UserAction::SaveReceivedFile { .. } => Vec::new(),
            UserAction::DismissChatNotice => self.with_room(|room| {
                room.chat.notice = None;
                Vec::new()
            }),
            UserAction::OpenSettings => {
                self.audio.open = true;
                vec![UiCommand::RefreshAudioDevices]
            }
            UserAction::CloseSettings => {
                self.audio.open = false;
                self.hotkeys.capturing = None;
                self.hotkeys.capture_error = None;
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
                    HotkeyAction::ToggleMute => self.with_room(|room| room.toggle_mute(voice_mode)),
                    HotkeyAction::ToggleDeafen => {
                        self.with_room(|room| room.toggle_deafen(voice_mode))
                    }
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
            UserAction::OpenSharePicker => self.open_picker(),
            UserAction::CloseSharePicker => {
                self.picker.open = false;
                Vec::new()
            }
            UserAction::RefreshCaptureTargets => vec![UiCommand::RefreshCaptureTargets],
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
        }
    }

    fn with_room(&mut self, f: impl FnOnce(&mut RoomState) -> Vec<UiCommand>) -> Vec<UiCommand> {
        match &mut self.screen {
            Screen::InRoom(state) => f(state),
            _ => Vec::new(),
        }
    }

    pub fn set_chat_notice(&mut self, text: String) {
        if let Screen::InRoom(room) = &mut self.screen {
            room.chat.set_notice(text);
        }
    }

    fn submit_login(&mut self) -> Vec<UiCommand> {
        if !matches!(self.screen, Screen::Login) || !self.form.is_valid() {
            return Vec::new();
        }
        let settings = self.settings_snapshot();
        let room = settings.last_room.clone();
        let mut commands = Vec::new();
        if let Some(id) = settings.input_device.clone() {
            commands.push(UiCommand::SetInputDevice(Some(id)));
        }
        if let Some(id) = settings.output_device.clone() {
            commands.push(UiCommand::SetOutputDevice(Some(id)));
        }
        if self.audio.stream_volume != FULL_VOLUME {
            commands.push(UiCommand::SetStreamVolume(self.audio.stream_volume));
        }
        if settings.noise_suppression != NoiseMode::default() {
            commands.push(UiCommand::SetNoiseSuppression(settings.noise_suppression));
        }
        if let Some(threshold) = settings.noise_gate_db {
            commands.push(UiCommand::SetNoiseGate(Some(threshold)));
        }
        if settings.muted_on_join || settings.voice_mode == VoiceMode::PushToTalk {
            commands.push(UiCommand::SetMuted(true));
        }
        commands.push(UiCommand::Join { room: room.clone() });
        self.pending_save = Some(settings);
        self.audio.open = false;
        self.screen = Screen::Connecting { room };
        commands
    }

    fn open_picker(&mut self) -> Vec<UiCommand> {
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        if room.share.local != LocalShare::Idle {
            return Vec::new();
        }
        room.share.error = None;
        self.picker.open = true;
        vec![UiCommand::RefreshCaptureTargets]
    }

    fn start_share(&mut self) -> Vec<UiCommand> {
        if !self.picker.can_start() {
            return Vec::new();
        }
        let Some(target_id) = self.picker.selected.clone() else {
            return Vec::new();
        };
        let preset = self.picker.preset;
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        if room.share.local != LocalShare::Idle {
            return Vec::new();
        }
        room.share.start();
        self.picker.open = false;
        vec![UiCommand::StartScreenShare { target_id, preset }]
    }

    pub fn is_watching(&self) -> bool {
        match &self.screen {
            Screen::InRoom(room) => room.share.watching.is_some(),
            _ => false,
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        match &self.screen {
            Screen::InRoom(room) => room.share.watching.as_ref().is_some_and(|w| w.fullscreen),
            _ => false,
        }
    }

    fn select_input(&mut self, id: Option<String>) -> Vec<UiCommand> {
        if self.audio.input == id {
            return Vec::new();
        }
        self.audio.input = id.clone();
        self.pending_save = Some(self.settings_snapshot());
        vec![UiCommand::SetInputDevice(id)]
    }

    fn select_output(&mut self, id: Option<String>) -> Vec<UiCommand> {
        if self.audio.output == id {
            return Vec::new();
        }
        self.audio.output = id.clone();
        self.pending_save = Some(self.settings_snapshot());
        vec![UiCommand::SetOutputDevice(id)]
    }

    fn select_noise(&mut self, mode: NoiseMode) -> Vec<UiCommand> {
        if self.audio.noise == mode {
            return Vec::new();
        }
        self.audio.noise = mode;
        if !mode.uses_own_capture() {
            self.audio.mic_level = None;
        }
        self.pending_save = Some(self.settings_snapshot());
        vec![UiCommand::SetNoiseSuppression(mode)]
    }

    fn select_voice_mode(&mut self, mode: VoiceMode) -> Vec<UiCommand> {
        if self.audio.voice_mode == mode {
            return Vec::new();
        }
        self.audio.voice_mode = mode;
        self.pending_save = Some(self.settings_snapshot());
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        room.ptt_held = false;
        room.ptt_release_at = None;
        room.sync_mic(mode).into_iter().collect()
    }

    fn set_hotkey(&mut self, action: HotkeyAction, binding: Option<String>) {
        if let Some(ref b) = binding
            && let Some(other) = HotkeyAction::ALL
                .into_iter()
                .find(|&other| other != action && self.hotkeys.binding(other) == Some(b.as_str()))
        {
            self.hotkeys.capture_error = Some(format!(
                "Esse atalho já está em uso em \"{}\".",
                other.label()
            ));
            return;
        }
        match action {
            HotkeyAction::ToggleMute => self.hotkeys.mute = binding,
            HotkeyAction::ToggleDeafen => self.hotkeys.deafen = binding,
            HotkeyAction::PushToTalk => self.hotkeys.ptt = binding,
        }
        self.hotkeys.capturing = None;
        self.hotkeys.capture_error = None;
        self.hotkeys.failed.retain(|&a| a != action);
        self.pending_save = Some(self.settings_snapshot());
    }

    fn cancel_connect(&mut self) -> Vec<UiCommand> {
        if matches!(self.screen, Screen::Connecting { .. }) {
            self.screen = Screen::Login;
            vec![UiCommand::Leave]
        } else {
            Vec::new()
        }
    }

    fn leave(&mut self) -> Vec<UiCommand> {
        self.picker.open = false;
        self.audio.mic_level = None;
        if matches!(self.screen, Screen::InRoom(_)) {
            self.screen = Screen::Login;
            vec![UiCommand::Leave]
        } else {
            Vec::new()
        }
    }

    fn switch_room(&mut self, room: String) -> Vec<UiCommand> {
        let Screen::InRoom(state) = &mut self.screen else {
            return Vec::new();
        };
        if state.room == room {
            return Vec::new();
        }
        state.reset_for_room(room.clone());
        state.status = ConnectionStatus::Connecting;
        self.form.room = room.clone();
        self.pending_save = Some(self.settings_snapshot());
        vec![UiCommand::Join { room }]
    }

    pub fn apply_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::RoomsUpdated(rooms) => self.rooms = rooms,
            UiEvent::AudioDevicesUpdated {
                inputs, outputs, ..
            } => {
                self.audio.inputs = inputs;
                self.audio.outputs = outputs;
                self.audio.loaded = true;
            }
            UiEvent::Joined { room, participants } => self.on_joined(room, participants),
            UiEvent::CaptureTargetsUpdated(targets) => self.picker.set_targets(targets),
            UiEvent::Failed { message } => {
                self.picker.open = false;
                self.audio.mic_level = None;
                if !matches!(self.screen, Screen::Login) {
                    self.screen = Screen::Error { message };
                }
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
                self.picker.open = false;
                self.audio.mic_level = None;
                if matches!(self.screen, Screen::InRoom(_) | Screen::Connecting { .. }) {
                    self.screen = Screen::Login;
                }
            }
            other => {
                if let Screen::InRoom(state) = &mut self.screen {
                    apply_room_event(state, other);
                }
            }
        }
    }

    fn on_joined(&mut self, room: String, participants: Vec<ParticipantView>) {
        match &mut self.screen {
            Screen::Connecting { .. } => {
                let state = RoomState::new(
                    room,
                    participants,
                    self.form.muted_on_join,
                    self.audio.voice_mode,
                );
                self.screen = Screen::InRoom(Box::new(state));
                self.last_refresh = None;
            }
            Screen::InRoom(state) => {
                if state.room != room {
                    state.reset_for_room(room);
                }
                state.participants = participants;
                state.status = ConnectionStatus::Connected;
                state.share = ShareState::default();
            }
            Screen::Login | Screen::Error { .. } => {}
        }
    }
}

fn apply_room_event(state: &mut RoomState, event: UiEvent) {
    match event {
        UiEvent::Connecting => state.status = ConnectionStatus::Connecting,
        UiEvent::Reconnecting => state.status = ConnectionStatus::Reconnecting,
        UiEvent::Reconnected => state.status = ConnectionStatus::Connected,
        UiEvent::ParticipantJoined(p) | UiEvent::ParticipantUpdated(p) => {
            state.upsert_participant(p);
        }
        UiEvent::ParticipantLeft { identity } => {
            state.participants.retain(|p| p.identity != identity);
            state.share.apply(UiEvent::ParticipantLeft { identity });
        }
        UiEvent::ChatReceived(message) => state.chat.push_message(message),
        UiEvent::FileTransfer(view) => state.chat.apply_transfer(view),
        share @ (UiEvent::ScreenShareStarted
        | UiEvent::ScreenShareStopped
        | UiEvent::ShareFailed { .. }
        | UiEvent::RemoteScreenShareStarted { .. }
        | UiEvent::RemoteScreenShareStopped { .. }
        | UiEvent::WatchingStarted { .. }
        | UiEvent::WatchingStopped) => state.share.apply(share),
        UiEvent::Joined { .. }
        | UiEvent::Left
        | UiEvent::Failed { .. }
        | UiEvent::RoomsUpdated(_)
        | UiEvent::CaptureTargetsUpdated(_)
        | UiEvent::MicLevel { .. }
        | UiEvent::AudioDevicesUpdated { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_model::TransferStateView;

    fn valid_settings() -> Settings {
        Settings {
            display_name: "Pedro".to_owned(),
            token_server_url: "http://localhost:8080".to_owned(),
            last_room: "geral".to_owned(),
            muted_on_join: false,
            input_device: None,
            output_device: None,
            noise_suppression: NoiseMode::default(),
            voice_mode: VoiceMode::default(),
            stream_volume: None,
            noise_gate_db: None,
            mute_hotkey: None,
            deafen_hotkey: None,
            ptt_hotkey: None,
        }
    }

    #[test]
    fn stream_volume_applies_while_moving_and_saves_when_asked() {
        let mut state = AppState::new(&valid_settings());
        assert_eq!(state.audio.stream_volume, FULL_VOLUME);
        assert_eq!(
            state.handle_action(UserAction::SetStreamVolume(40)),
            vec![UiCommand::SetStreamVolume(40)]
        );
        assert!(state.take_pending_save().is_none());
        assert!(
            state
                .handle_action(UserAction::SetStreamVolume(40))
                .is_empty()
        );
        state.handle_action(UserAction::SaveAudioSettings);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.stream_volume, Some(40));

        let mut restored = AppState::new(&saved);
        restored.form.invite_code = "segredo".to_owned();
        let commands = restored.handle_action(UserAction::SubmitLogin);
        assert!(commands.contains(&UiCommand::SetStreamVolume(40)));
    }

    #[test]
    fn noise_gate_applies_without_saving_then_saves_and_restores_on_login() {
        let mut state = AppState::new(&valid_settings());
        assert_eq!(state.audio.noise_gate, None);
        assert_eq!(
            state.handle_action(UserAction::SetNoiseGate(Some(-45))),
            vec![UiCommand::SetNoiseGate(Some(-45))]
        );
        assert!(state.take_pending_save().is_none());
        assert!(
            state
                .handle_action(UserAction::SetNoiseGate(Some(-45)))
                .is_empty()
        );
        state.handle_action(UserAction::SaveAudioSettings);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.noise_gate_db, Some(-45));

        let mut restored = AppState::new(&saved);
        restored.form.invite_code = "segredo".to_owned();
        let commands = restored.handle_action(UserAction::SubmitLogin);
        assert!(commands.contains(&UiCommand::SetNoiseGate(Some(-45))));

        let mut off = AppState::new(&valid_settings());
        off.form.invite_code = "segredo".to_owned();
        let commands = off.handle_action(UserAction::SubmitLogin);
        assert!(
            !commands
                .iter()
                .any(|c| matches!(c, UiCommand::SetNoiseGate(_)))
        );
    }

    #[test]
    fn mic_level_follows_events_and_clears_on_leave_and_mode_change() {
        let mut settings = valid_settings();
        settings.noise_suppression = NoiseMode::Enhanced;
        let mut state = AppState::new(&settings);
        state.form.invite_code = "segredo".to_owned();
        let level = UiEvent::MicLevel {
            level_db: -38,
            gate_open: true,
        };
        state.apply_event(level.clone());
        assert_eq!(state.audio.mic_level, None);

        in_room(&mut state);
        state.apply_event(level.clone());
        assert_eq!(
            state.audio.mic_level,
            Some(MicLevelView {
                level_db: -38,
                gate_open: true
            })
        );

        state.handle_action(UserAction::SelectNoiseMode(NoiseMode::Standard));
        assert_eq!(state.audio.mic_level, None);
        state.apply_event(level.clone());
        assert_eq!(state.audio.mic_level, None);

        state.handle_action(UserAction::SelectNoiseMode(NoiseMode::Enhanced));
        state.apply_event(level.clone());
        assert!(state.audio.mic_level.is_some());
        state.apply_event(UiEvent::Left);
        assert_eq!(state.audio.mic_level, None);

        in_room(&mut state);
        state.apply_event(level.clone());
        state.handle_action(UserAction::Leave);
        assert_eq!(state.audio.mic_level, None);

        in_room(&mut state);
        state.apply_event(level);
        state.apply_event(UiEvent::Failed {
            message: "x".to_owned(),
        });
        assert_eq!(state.audio.mic_level, None);
    }

    #[test]
    fn maximum_mode_shows_the_mic_level_and_is_sent_and_saved() {
        let mut state = AppState::new(&valid_settings());
        state.form.invite_code = "segredo".to_owned();
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::SelectNoiseMode(NoiseMode::Maximum)),
            vec![UiCommand::SetNoiseSuppression(NoiseMode::Maximum)]
        );
        state.apply_event(UiEvent::MicLevel {
            level_db: -30,
            gate_open: false,
        });
        assert!(state.audio.mic_level.is_some());
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.noise_suppression, NoiseMode::Maximum);

        state.handle_action(UserAction::SelectNoiseMode(NoiseMode::Enhanced));
        assert!(state.audio.mic_level.is_some());
        state.handle_action(UserAction::SelectNoiseMode(NoiseMode::Off));
        assert_eq!(state.audio.mic_level, None);
    }

    #[test]
    fn noise_mode_is_sent_saved_and_restored_on_login() {
        let mut state = AppState::new(&valid_settings());
        assert_eq!(
            state.handle_action(UserAction::SelectNoiseMode(NoiseMode::Enhanced)),
            vec![UiCommand::SetNoiseSuppression(NoiseMode::Enhanced)]
        );
        assert!(
            state
                .handle_action(UserAction::SelectNoiseMode(NoiseMode::Enhanced))
                .is_empty()
        );
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.noise_suppression, NoiseMode::Enhanced);

        let mut restored = AppState::new(&saved);
        restored.form.invite_code = "segredo".to_owned();
        let commands = restored.handle_action(UserAction::SubmitLogin);
        assert!(commands.contains(&UiCommand::SetNoiseSuppression(NoiseMode::Enhanced)));
    }

    fn mic(id: &str) -> AudioDeviceView {
        AudioDeviceView {
            id: id.to_owned(),
            name: format!("Mic {id}"),
        }
    }

    fn devices_event(inputs: &[&str], outputs: &[&str]) -> UiEvent {
        UiEvent::AudioDevicesUpdated {
            inputs: inputs.iter().map(|id| mic(id)).collect(),
            outputs: outputs.iter().map(|id| mic(id)).collect(),
            selected_input: None,
            selected_output: None,
        }
    }

    fn state_with_code() -> AppState {
        let mut state = AppState::new(&valid_settings());
        state.form.invite_code = "segredo".to_owned();
        state
    }

    fn person(identity: &str) -> ParticipantView {
        ParticipantView {
            identity: identity.to_owned(),
            name: identity.to_uppercase(),
            muted: false,
            speaking: false,
        }
    }

    fn message(id: usize) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            sender: "ana".to_owned(),
            sent_at_unix_ms: 0,
            text: format!("msg {id}"),
        }
    }

    fn transfer(id: &str, outgoing: bool, state: TransferStateView) -> FileTransferView {
        FileTransferView {
            id: id.to_owned(),
            sender: "ana".to_owned(),
            name: "foto.png".to_owned(),
            size: 100,
            is_image: true,
            outgoing,
            sent_at_unix_ms: 0,
            state,
        }
    }

    fn in_room(state: &mut AppState) {
        state.handle_action(UserAction::SubmitLogin);
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            participants: vec![person("ana")],
        });
    }

    fn room(state: &AppState) -> &RoomState {
        match &state.screen {
            Screen::InRoom(room) => room,
            other => panic!("expected InRoom, got {other:?}"),
        }
    }

    #[test]
    fn valid_form_has_no_errors() {
        assert!(state_with_code().form.is_valid());
    }

    #[test]
    fn each_invalid_field_is_reported() {
        let mut form = state_with_code().form;
        form.display_name = "  ".to_owned();
        assert_eq!(
            form.errors().display_name.as_deref(),
            Some("Informe seu nome.")
        );
        assert!(form.errors().room.is_none());

        let mut form = state_with_code().form;
        form.display_name = "a".repeat(33);
        assert!(form.errors().display_name.is_some());

        let mut form = state_with_code().form;
        form.room = "Sala Grande".to_owned();
        assert!(form.errors().room.is_some());
        form.room = String::new();
        assert_eq!(form.errors().room.as_deref(), Some("Informe a sala."));

        let mut form = state_with_code().form;
        form.invite_code = "   ".to_owned();
        assert!(form.errors().invite_code.is_some());

        for bad in ["", "localhost:8080", "ftp://x", "http://", "http://a b"] {
            let mut form = state_with_code().form;
            form.server_url = bad.to_owned();
            assert!(form.errors().server_url.is_some(), "{bad:?}");
        }
        for good in [
            "http://localhost:8080",
            "https://rooms.example.com",
            " https://x ",
        ] {
            let mut form = state_with_code().form;
            form.server_url = good.to_owned();
            assert!(form.errors().server_url.is_none(), "{good:?}");
        }
    }

    #[test]
    fn invalid_form_does_not_submit() {
        let mut state = AppState::new(&valid_settings());
        assert!(state.handle_action(UserAction::SubmitLogin).is_empty());
        assert_eq!(state.screen, Screen::Login);
        assert!(state.take_pending_save().is_none());
    }

    #[test]
    fn join_flow_goes_login_connecting_in_room() {
        let mut state = state_with_code();
        let commands = state.handle_action(UserAction::SubmitLogin);
        assert_eq!(
            commands,
            vec![UiCommand::Join {
                room: "geral".to_owned()
            }]
        );
        assert_eq!(
            state.screen,
            Screen::Connecting {
                room: "geral".to_owned()
            }
        );
        assert_eq!(state.take_pending_save(), Some(valid_settings()));
        assert!(state.take_pending_save().is_none());

        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            participants: vec![person("ana")],
        });
        let room = room(&state);
        assert_eq!(room.room, "geral");
        assert_eq!(room.participants.len(), 1);
        assert_eq!(room.status, ConnectionStatus::Connected);
    }

    #[test]
    fn muted_on_join_sends_mute_first_and_starts_muted() {
        let mut state = state_with_code();
        state.form.muted_on_join = true;
        let commands = state.handle_action(UserAction::SubmitLogin);
        assert_eq!(
            commands,
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            participants: vec![],
        });
        assert!(room(&state).muted);
    }

    #[test]
    fn failure_shows_error_and_retry_keeps_fields() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitLogin);
        state.apply_event(UiEvent::Failed {
            message: "sem rede".to_owned(),
        });
        assert_eq!(
            state.screen,
            Screen::Error {
                message: "sem rede".to_owned()
            }
        );
        state.handle_action(UserAction::Retry);
        assert_eq!(state.screen, Screen::Login);
        assert_eq!(state.form.display_name, "Pedro");
        assert_eq!(state.form.invite_code, "segredo");
        assert_eq!(state.form.room, "geral");
    }

    #[test]
    fn stale_events_are_ignored_on_login() {
        let mut state = state_with_code();
        state.apply_event(UiEvent::Failed {
            message: "x".to_owned(),
        });
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            participants: vec![],
        });
        assert_eq!(state.screen, Screen::Login);
    }

    #[test]
    fn cancel_connect_returns_to_login_and_leaves() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitLogin);
        assert_eq!(
            state.handle_action(UserAction::CancelConnect),
            vec![UiCommand::Leave]
        );
        assert_eq!(state.screen, Screen::Login);
    }

    #[test]
    fn leave_and_left_event_return_to_login() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::Leave),
            vec![UiCommand::Leave]
        );
        assert_eq!(state.screen, Screen::Login);

        in_room(&mut state);
        state.apply_event(UiEvent::Left);
        assert_eq!(state.screen, Screen::Login);
    }

    #[test]
    fn participants_join_leave_and_update() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ParticipantJoined(person("bruno")));
        assert_eq!(room(&state).participants.len(), 2);

        let mut updated = person("ana");
        updated.muted = true;
        updated.speaking = true;
        state.apply_event(UiEvent::ParticipantUpdated(updated.clone()));
        assert_eq!(room(&state).participants[0], updated);
        assert_eq!(room(&state).participants.len(), 2);

        state.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert_eq!(room(&state).participants, vec![person("bruno")]);
    }

    #[test]
    fn reconnecting_updates_status() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::Reconnecting);
        assert_eq!(room(&state).status, ConnectionStatus::Reconnecting);
        state.apply_event(UiEvent::Reconnected);
        assert_eq!(room(&state).status, ConnectionStatus::Connected);
    }

    #[test]
    fn mute_toggles_and_emits_command() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert!(room(&state).muted);
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(false), UiCommand::PlayCue(Cue::Unmuted)]
        );
    }

    #[test]
    fn deafen_mutes_and_undeafen_restores_previous_mute_state() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened)
            ]
        );
        assert!(room(&state).deafened && room(&state).muted);
        assert!(state.handle_action(UserAction::ToggleMute).is_empty());
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(false),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
        assert!(!room(&state).muted && !room(&state).deafened);

        state.handle_action(UserAction::ToggleMute);
        state.handle_action(UserAction::ToggleDeafen);
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
        assert!(room(&state).muted);
    }

    #[test]
    fn switching_room_clears_chat_and_emits_join() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(message(1)));
        state.take_pending_save();
        let commands = state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        assert_eq!(
            commands,
            vec![UiCommand::Join {
                room: "jogos".to_owned()
            }]
        );
        let current = room(&state);
        assert_eq!(current.room, "jogos");
        assert!(current.chat.messages.is_empty());
        assert!(current.participants.is_empty());
        assert_eq!(current.status, ConnectionStatus::Connecting);
        assert_eq!(state.form.room, "jogos");
        assert_eq!(
            state.take_pending_save().map(|s| s.last_room),
            Some("jogos".to_owned())
        );

        assert!(
            state
                .handle_action(UserAction::SwitchRoom("jogos".to_owned()))
                .is_empty()
        );
    }

    #[test]
    fn joined_with_other_room_clears_chat() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(message(1)));
        state.apply_event(UiEvent::Joined {
            room: "musica".to_owned(),
            participants: vec![],
        });
        assert_eq!(room(&state).room, "musica");
        assert!(room(&state).chat.messages.is_empty());
    }

    #[test]
    fn unread_counts_only_while_chat_is_collapsed() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(message(1)));
        assert_eq!(room(&state).chat.unread, 0);

        state.handle_action(UserAction::ToggleChat);
        assert!(!room(&state).chat.open);
        state.apply_event(UiEvent::ChatReceived(message(2)));
        state.apply_event(UiEvent::ChatReceived(message(3)));
        assert_eq!(room(&state).chat.unread, 2);

        state.handle_action(UserAction::ToggleChat);
        assert_eq!(room(&state).chat.unread, 0);
    }

    #[test]
    fn file_transfer_is_added_then_updated_by_id() {
        let mut state = state_with_code();
        in_room(&mut state);
        let progress = TransferStateView::InProgress { bytes: 10 };
        state.apply_event(UiEvent::FileTransfer(transfer("t1", false, progress)));
        state.apply_event(UiEvent::ChatReceived(message(1)));
        let done = TransferStateView::Done {
            path: PathBuf::from("a.png"),
        };
        state.apply_event(UiEvent::FileTransfer(transfer("t1", false, done.clone())));
        let chat = &room(&state).chat;
        assert_eq!(chat.messages.len(), 2);
        assert!(matches!(
            chat.messages.front(),
            Some(ChatEntry::File(f)) if f.state == done
        ));
    }

    #[test]
    fn file_transfer_unread_counts_only_incoming_while_collapsed() {
        let mut state = state_with_code();
        in_room(&mut state);
        let progress = TransferStateView::InProgress { bytes: 0 };
        state.apply_event(UiEvent::FileTransfer(transfer(
            "t0",
            false,
            progress.clone(),
        )));
        assert_eq!(room(&state).chat.unread, 0);
        state.handle_action(UserAction::ToggleChat);
        state.apply_event(UiEvent::FileTransfer(transfer(
            "t1",
            false,
            progress.clone(),
        )));
        state.apply_event(UiEvent::FileTransfer(transfer(
            "t1",
            false,
            progress.clone(),
        )));
        state.apply_event(UiEvent::FileTransfer(transfer("t2", true, progress)));
        assert_eq!(room(&state).chat.unread, 1);
        assert_eq!(room(&state).chat.messages.len(), 3);
    }

    #[test]
    fn attach_file_needs_a_room() {
        let mut state = state_with_code();
        let action = UserAction::AttachFile(PathBuf::from("a.png"));
        assert!(state.handle_action(action.clone()).is_empty());
        in_room(&mut state);
        assert_eq!(
            state.handle_action(action),
            vec![UiCommand::SendFile {
                path: PathBuf::from("a.png")
            }]
        );
    }

    #[test]
    fn effect_actions_do_not_change_state() {
        let mut state = state_with_code();
        in_room(&mut state);
        let before = room(&state).clone();
        assert!(state.handle_action(UserAction::PickFile).is_empty());
        let save = UserAction::SaveReceivedFile {
            source: PathBuf::from("a.png"),
            name: "a.png".to_owned(),
        };
        assert!(state.handle_action(save).is_empty());
        assert_eq!(*room(&state), before);
    }

    #[test]
    fn chat_notice_is_set_dismissed_and_cleared_on_room_switch() {
        let mut state = state_with_code();
        state.set_chat_notice("ignorado".to_owned());
        in_room(&mut state);
        state.set_chat_notice("Arquivo salvo".to_owned());
        assert_eq!(room(&state).chat.notice.as_deref(), Some("Arquivo salvo"));
        state.handle_action(UserAction::DismissChatNotice);
        assert_eq!(room(&state).chat.notice, None);
        state.set_chat_notice("outro".to_owned());
        state.apply_event(UiEvent::FileTransfer(transfer(
            "t1",
            false,
            TransferStateView::Failed("x".to_owned()),
        )));
        state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        let chat = &room(&state).chat;
        assert!(chat.messages.is_empty());
        assert_eq!(chat.notice, None);
    }

    #[test]
    fn chat_history_is_capped() {
        let mut state = state_with_code();
        in_room(&mut state);
        for id in 0..MAX_CHAT_HISTORY + 25 {
            state.apply_event(UiEvent::ChatReceived(message(id)));
        }
        let chat = &room(&state).chat;
        assert_eq!(chat.messages.len(), MAX_CHAT_HISTORY);
        assert!(matches!(
            chat.messages.front(),
            Some(ChatEntry::Text(m)) if m.id == "25"
        ));
    }

    #[test]
    fn send_chat_trims_clears_draft_and_rejects_empty() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert!(state.handle_action(UserAction::SendChat).is_empty());

        if let Screen::InRoom(room) = &mut state.screen {
            room.chat.draft = "  oi  ".to_owned();
        }
        assert_eq!(
            state.handle_action(UserAction::SendChat),
            vec![UiCommand::SendChat {
                text: "oi".to_owned()
            }]
        );
        assert!(room(&state).chat.draft.is_empty());

        if let Screen::InRoom(room) = &mut state.screen {
            room.chat.draft = "x".repeat(MAX_CHAT_MESSAGE_LEN + 1);
        }
        assert!(state.handle_action(UserAction::SendChat).is_empty());
    }

    #[test]
    fn chat_counter_appears_near_the_limit() {
        let mut chat = ChatState::new();
        chat.draft = "x".repeat(CHAT_COUNTER_THRESHOLD - 1);
        assert!(!chat.show_counter());
        chat.draft.push('x');
        assert!(chat.show_counter());
    }

    #[test]
    fn rooms_updated_replaces_the_list() {
        let mut state = state_with_code();
        let summary = |name: &str| RoomSummary {
            name: name.to_owned(),
            participants: vec![],
        };
        state.apply_event(UiEvent::RoomsUpdated(vec![summary("a"), summary("b")]));
        assert_eq!(state.rooms.len(), 2);
        state.apply_event(UiEvent::RoomsUpdated(vec![summary("c")]));
        assert_eq!(state.rooms, vec![summary("c")]);
    }

    #[test]
    fn refresh_is_driven_by_elapsed_time_only_in_room() {
        let mut state = state_with_code();
        let start = Instant::now();
        assert!(state.poll_refresh(start).is_none());
        in_room(&mut state);
        assert_eq!(state.poll_refresh(start), Some(UiCommand::RefreshRooms));
        assert!(state.poll_refresh(start).is_none());
        assert!(state.poll_refresh(start + Duration::from_secs(2)).is_none());
        assert_eq!(
            state.poll_refresh(start + ROOMS_REFRESH_INTERVAL),
            Some(UiCommand::RefreshRooms)
        );
    }

    #[test]
    fn opening_settings_requests_the_device_list() {
        let mut state = state_with_code();
        assert_eq!(
            state.handle_action(UserAction::OpenSettings),
            vec![UiCommand::RefreshAudioDevices]
        );
        assert!(state.audio.open);
        assert!(state.handle_action(UserAction::CloseSettings).is_empty());
        assert!(!state.audio.open);
        assert_eq!(
            state.handle_action(UserAction::RefreshAudioDevices),
            vec![UiCommand::RefreshAudioDevices]
        );
    }

    #[test]
    fn choosing_a_device_emits_the_command_and_marks_settings_for_saving() {
        let mut state = state_with_code();
        assert_eq!(
            state.handle_action(UserAction::SelectInputDevice(Some("m1".to_owned()))),
            vec![UiCommand::SetInputDevice(Some("m1".to_owned()))]
        );
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.input_device.as_deref(), Some("m1"));
        assert_eq!(saved.output_device, None);
        assert_eq!(saved.display_name, "Pedro");

        assert_eq!(
            state.handle_action(UserAction::SelectOutputDevice(Some("o1".to_owned()))),
            vec![UiCommand::SetOutputDevice(Some("o1".to_owned()))]
        );
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.input_device.as_deref(), Some("m1"));
        assert_eq!(saved.output_device.as_deref(), Some("o1"));

        assert_eq!(
            state.handle_action(UserAction::SelectInputDevice(None)),
            vec![UiCommand::SetInputDevice(None)]
        );
        assert_eq!(state.take_pending_save().unwrap().input_device, None);
    }

    #[test]
    fn choosing_the_current_device_again_does_nothing() {
        let mut state = state_with_code();
        assert!(
            state
                .handle_action(UserAction::SelectInputDevice(None))
                .is_empty()
        );
        assert!(state.take_pending_save().is_none());
        state.handle_action(UserAction::SelectOutputDevice(Some("o1".to_owned())));
        state.take_pending_save();
        assert!(
            state
                .handle_action(UserAction::SelectOutputDevice(Some("o1".to_owned())))
                .is_empty()
        );
        assert!(state.take_pending_save().is_none());
    }

    #[test]
    fn device_choice_works_inside_a_room_and_is_kept_when_switching_rooms() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::SelectOutputDevice(Some("o1".to_owned()))),
            vec![UiCommand::SetOutputDevice(Some("o1".to_owned()))]
        );
        state.take_pending_save();
        state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.output_device.as_deref(), Some("o1"));
        assert_eq!(saved.last_room, "jogos");
    }

    #[test]
    fn audio_devices_updated_replaces_the_lists_and_ignores_the_backend_selection() {
        let mut state = AppState::new(&Settings {
            input_device: Some("m1".to_owned()),
            ..valid_settings()
        });
        state.apply_event(devices_event(&["m1", "m2"], &["o1"]));
        assert_eq!(state.audio.inputs, vec![mic("m1"), mic("m2")]);
        assert_eq!(state.audio.outputs, vec![mic("o1")]);
        state.apply_event(devices_event(&["m3"], &[]));
        assert_eq!(state.audio.inputs, vec![mic("m3")]);
        assert!(state.audio.outputs.is_empty());
        assert_eq!(state.audio.input.as_deref(), Some("m1"));
        assert_eq!(state.screen, Screen::Login);
    }

    #[test]
    fn audio_devices_updated_does_not_disturb_the_room() {
        let mut state = state_with_code();
        in_room(&mut state);
        let before = room(&state).clone();
        state.apply_event(devices_event(&["m1"], &["o1"]));
        assert_eq!(room(&state), &before);
    }

    #[test]
    fn selected_label_covers_default_present_missing_and_not_loaded() {
        let mut state = AppState::new(&Settings {
            input_device: Some("m1".to_owned()),
            ..valid_settings()
        });
        assert_eq!(state.audio.output_label(), DEFAULT_DEVICE_LABEL);
        assert_eq!(state.audio.input_label(), LOADING_DEVICE_LABEL);

        state.apply_event(devices_event(&["m1"], &[]));
        assert_eq!(state.audio.input_label(), "Mic m1");

        state.apply_event(devices_event(&["m2"], &[]));
        assert_eq!(state.audio.input_label(), MISSING_DEVICE_LABEL);
        assert_eq!(state.audio.input.as_deref(), Some("m1"));

        state.handle_action(UserAction::SelectInputDevice(None));
        assert_eq!(state.audio.input_label(), DEFAULT_DEVICE_LABEL);
    }

    #[test]
    fn login_sends_saved_devices_before_mute_and_join_and_closes_settings() {
        let mut state = AppState::new(&Settings {
            input_device: Some("m1".to_owned()),
            output_device: Some("o1".to_owned()),
            muted_on_join: true,
            ..valid_settings()
        });
        state.form.invite_code = "segredo".to_owned();
        state.handle_action(UserAction::OpenSettings);
        let commands = state.handle_action(UserAction::SubmitLogin);
        assert_eq!(
            commands,
            vec![
                UiCommand::SetInputDevice(Some("m1".to_owned())),
                UiCommand::SetOutputDevice(Some("o1".to_owned())),
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                },
            ]
        );
        assert!(!state.audio.open);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.input_device.as_deref(), Some("m1"));
        assert_eq!(saved.output_device.as_deref(), Some("o1"));
    }

    #[test]
    fn saved_device_choices_load_from_settings() {
        let state = AppState::new(&Settings {
            input_device: Some("m1".to_owned()),
            ..valid_settings()
        });
        assert_eq!(state.audio.input.as_deref(), Some("m1"));
        assert_eq!(state.audio.output, None);
    }

    struct NoFrames;

    impl crate::ui_model::FrameFeed for NoFrames {
        fn take_newer(&self, _last_seen: &mut u64) -> Option<crate::ui_model::UiFrame> {
            None
        }
    }

    fn feed() -> FeedHandle {
        FeedHandle(std::sync::Arc::new(NoFrames))
    }

    fn target(id: &str, kind: TargetKindView) -> CaptureTargetView {
        CaptureTargetView {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
        }
    }

    fn targets_event() -> UiEvent {
        UiEvent::CaptureTargetsUpdated(vec![
            target("w1", TargetKindView::Window),
            target("s1", TargetKindView::Screen),
        ])
    }

    fn remote_share(state: &mut AppState, identity: &str) {
        state.apply_event(UiEvent::RemoteScreenShareStarted {
            identity: identity.to_owned(),
        });
    }

    #[test]
    fn picker_opens_only_in_a_room_and_requests_targets() {
        let mut state = state_with_code();
        assert!(state.handle_action(UserAction::OpenSharePicker).is_empty());
        assert!(!state.picker.open);
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::OpenSharePicker),
            vec![UiCommand::RefreshCaptureTargets]
        );
        assert!(state.picker.open);
    }

    #[test]
    fn picker_preselects_first_screen_and_starts_share() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.handle_action(UserAction::OpenSharePicker);
        assert!(state.handle_action(UserAction::StartShare).is_empty());
        state.apply_event(targets_event());
        assert_eq!(state.picker.selected.as_deref(), Some("s1"));
        state.handle_action(UserAction::SelectCaptureTarget("w1".to_owned()));
        state.handle_action(UserAction::SelectCaptureTarget("zzz".to_owned()));
        state.handle_action(UserAction::SelectSharePreset(SharePresetView::P720F30));
        assert_eq!(
            state.handle_action(UserAction::StartShare),
            vec![UiCommand::StartScreenShare {
                target_id: "w1".to_owned(),
                preset: SharePresetView::P720F30
            }]
        );
        assert!(!state.picker.open);
        assert_eq!(room(&state).share.local, LocalShare::Starting);
        assert!(state.handle_action(UserAction::StartShare).is_empty());
        assert!(state.handle_action(UserAction::OpenSharePicker).is_empty());

        state.apply_event(UiEvent::ScreenShareStarted);
        assert_eq!(room(&state).share.local, LocalShare::Active);
        assert_eq!(
            state.handle_action(UserAction::StopShare),
            vec![UiCommand::StopScreenShare]
        );
        state.apply_event(UiEvent::ScreenShareStopped);
        assert_eq!(room(&state).share.local, LocalShare::Idle);
        assert!(state.handle_action(UserAction::StopShare).is_empty());
    }

    #[test]
    fn failed_share_start_returns_to_idle_with_error() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.handle_action(UserAction::OpenSharePicker);
        state.apply_event(targets_event());
        state.handle_action(UserAction::StartShare);
        state.apply_event(UiEvent::ShareFailed {
            message: "falhou".to_owned(),
        });
        let share = &room(&state).share;
        assert_eq!(share.local, LocalShare::Idle);
        assert_eq!(share.error.as_deref(), Some("falhou"));
        state.handle_action(UserAction::DismissShareError);
        assert_eq!(room(&state).share.error, None);
    }

    #[test]
    fn watch_flow_and_fullscreen() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert!(
            state
                .handle_action(UserAction::Watch("ana".to_owned()))
                .is_empty()
        );
        remote_share(&mut state, "ana");
        assert_eq!(
            state.handle_action(UserAction::Watch("ana".to_owned())),
            vec![UiCommand::WatchScreenShare {
                identity: "ana".to_owned()
            }]
        );
        state.apply_event(UiEvent::WatchingStarted {
            identity: "ana".to_owned(),
            feed: feed(),
        });
        assert!(state.is_watching());
        assert!(!state.is_fullscreen());
        state.handle_action(UserAction::ToggleFullscreen);
        assert!(state.is_fullscreen());
        assert!(
            state
                .handle_action(UserAction::Watch("ana".to_owned()))
                .is_empty()
        );
        assert_eq!(
            state.handle_action(UserAction::StopWatching),
            vec![UiCommand::StopWatching]
        );
        assert!(!state.is_watching());
        assert!(!state.is_fullscreen());
        assert!(state.handle_action(UserAction::StopWatching).is_empty());
    }

    #[test]
    fn remote_share_end_or_departure_stops_watching() {
        let mut state = state_with_code();
        in_room(&mut state);
        remote_share(&mut state, "ana");
        state.apply_event(UiEvent::WatchingStarted {
            identity: "ana".to_owned(),
            feed: feed(),
        });
        state.apply_event(UiEvent::RemoteScreenShareStopped {
            identity: "ana".to_owned(),
        });
        assert!(!state.is_watching());
        assert!(!room(&state).share.is_sharing_remote("ana"));

        remote_share(&mut state, "ana");
        state.apply_event(UiEvent::WatchingStarted {
            identity: "ana".to_owned(),
            feed: feed(),
        });
        state.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(!state.is_watching());
        assert!(room(&state).share.remote.is_empty());

        remote_share(&mut state, "bia");
        state.apply_event(UiEvent::WatchingStarted {
            identity: "bia".to_owned(),
            feed: feed(),
        });
        state.apply_event(UiEvent::WatchingStopped);
        assert!(!state.is_watching());
    }

    #[test]
    fn rejoining_resets_share_state_and_leaving_closes_picker() {
        let mut state = state_with_code();
        in_room(&mut state);
        remote_share(&mut state, "ana");
        state.apply_event(UiEvent::ScreenShareStarted);
        state.apply_event(UiEvent::Joined {
            room: "jogos".to_owned(),
            participants: vec![person("ana")],
        });
        assert_eq!(room(&state).share, ShareState::default());

        state.handle_action(UserAction::OpenSharePicker);
        assert!(state.picker.open);
        state.handle_action(UserAction::Leave);
        assert!(!state.picker.open);
    }

    #[test]
    fn aspect_fit_keeps_ratio() {
        assert_eq!(aspect_fit((800.0, 600.0), (1920, 1080)), (800.0, 450.0));
        let (w, h) = aspect_fit((1000.0, 300.0), (1920, 1080));
        assert!((w - 533.333).abs() < 0.01 && (h - 300.0).abs() < 0.01);
        assert_eq!(aspect_fit((0.0, 100.0), (1920, 1080)), (0.0, 0.0));
        assert_eq!(aspect_fit((100.0, 100.0), (0, 0)), (0.0, 0.0));
    }

    #[test]
    fn hotkeys_load_save_and_suspend_registrations_while_capturing() {
        let mut settings = valid_settings();
        settings.mute_hotkey = Some("control+KeyM".to_owned());
        settings.deafen_hotkey = Some("control+KeyD".to_owned());
        settings.ptt_hotkey = Some("F9".to_owned());
        let mut state = AppState::new(&settings);
        assert_eq!(
            state.hotkeys.binding(HotkeyAction::ToggleMute),
            Some("control+KeyM")
        );
        assert_eq!(
            state.hotkeys.binding(HotkeyAction::ToggleDeafen),
            Some("control+KeyD")
        );
        assert_eq!(state.hotkeys.binding(HotkeyAction::PushToTalk), Some("F9"));
        assert_eq!(
            state.hotkeys.registrations(false),
            vec![
                (HotkeyAction::ToggleMute, "control+KeyM".to_owned()),
                (HotkeyAction::ToggleDeafen, "control+KeyD".to_owned()),
            ]
        );
        assert_eq!(
            state.hotkeys.registrations(true),
            vec![
                (HotkeyAction::ToggleMute, "control+KeyM".to_owned()),
                (HotkeyAction::ToggleDeafen, "control+KeyD".to_owned()),
                (HotkeyAction::PushToTalk, "F9".to_owned()),
            ]
        );

        state.handle_action(UserAction::StartHotkeyCapture(HotkeyAction::ToggleMute));
        assert_eq!(state.hotkeys.capturing, Some(HotkeyAction::ToggleMute));
        assert!(state.hotkeys.registrations(false).is_empty());
        assert!(state.hotkeys.registrations(true).is_empty());

        state.handle_action(UserAction::RejectHotkey("erro".to_owned()));
        assert_eq!(state.hotkeys.capture_error.as_deref(), Some("erro"));
        assert_eq!(state.hotkeys.capturing, Some(HotkeyAction::ToggleMute));

        state.handle_action(UserAction::CancelHotkeyCapture);
        assert_eq!(state.hotkeys.capturing, None);
        assert_eq!(state.hotkeys.capture_error, None);
        assert_eq!(state.hotkeys.registrations(false).len(), 2);
        assert_eq!(state.hotkeys.registrations(true).len(), 3);

        state.handle_action(UserAction::OpenSettings);
        state.handle_action(UserAction::StartHotkeyCapture(HotkeyAction::ToggleDeafen));
        state.handle_action(UserAction::RejectHotkey("erro".to_owned()));
        state.handle_action(UserAction::CloseSettings);
        assert!(!state.audio.open);
        assert_eq!(state.hotkeys.capturing, None);
        assert_eq!(state.hotkeys.capture_error, None);
    }

    #[test]
    fn setting_removing_and_deduplicating_hotkeys_updates_state_and_failures() {
        let mut state = AppState::new(&valid_settings());
        state.set_hotkey_failures(vec![HotkeyAction::ToggleMute, HotkeyAction::ToggleDeafen]);
        state.handle_action(UserAction::StartHotkeyCapture(HotkeyAction::ToggleMute));
        state.handle_action(UserAction::RejectHotkey("antigo".to_owned()));
        state.handle_action(UserAction::SetHotkey(
            HotkeyAction::ToggleMute,
            Some("shift+KeyM".to_owned()),
        ));
        assert_eq!(state.hotkeys.mute.as_deref(), Some("shift+KeyM"));
        assert_eq!(state.hotkeys.capturing, None);
        assert_eq!(state.hotkeys.capture_error, None);
        assert_eq!(state.hotkeys.failed, vec![HotkeyAction::ToggleDeafen]);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.mute_hotkey.as_deref(), Some("shift+KeyM"));

        state.handle_action(UserAction::StartHotkeyCapture(HotkeyAction::ToggleDeafen));
        state.handle_action(UserAction::SetHotkey(
            HotkeyAction::ToggleDeafen,
            Some("shift+KeyM".to_owned()),
        ));
        assert_eq!(state.hotkeys.deafen, None);
        assert_eq!(state.hotkeys.capturing, Some(HotkeyAction::ToggleDeafen));
        assert_eq!(
            state.hotkeys.capture_error.as_deref(),
            Some("Esse atalho já está em uso em \"Mutar microfone\".")
        );
        assert!(state.take_pending_save().is_none());

        state.handle_action(UserAction::SetHotkey(
            HotkeyAction::ToggleDeafen,
            Some("shift+KeyD".to_owned()),
        ));
        assert_eq!(state.hotkeys.deafen.as_deref(), Some("shift+KeyD"));
        assert!(state.hotkeys.failed.is_empty());
        assert_eq!(
            state.take_pending_save().unwrap().deafen_hotkey.as_deref(),
            Some("shift+KeyD")
        );

        state.handle_action(UserAction::StartHotkeyCapture(HotkeyAction::PushToTalk));
        state.handle_action(UserAction::SetHotkey(
            HotkeyAction::PushToTalk,
            Some("shift+KeyD".to_owned()),
        ));
        assert_eq!(state.hotkeys.ptt, None);
        assert_eq!(
            state.hotkeys.capture_error.as_deref(),
            Some("Esse atalho já está em uso em \"Ensurdecer\".")
        );
        state.handle_action(UserAction::SetHotkey(
            HotkeyAction::PushToTalk,
            Some("F9".to_owned()),
        ));
        assert_eq!(state.hotkeys.ptt.as_deref(), Some("F9"));
        assert_eq!(
            state.take_pending_save().unwrap().ptt_hotkey.as_deref(),
            Some("F9")
        );

        state.handle_action(UserAction::SetHotkey(HotkeyAction::ToggleMute, None));
        assert_eq!(state.hotkeys.mute, None);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.mute_hotkey, None);
        assert_eq!(saved.deafen_hotkey.as_deref(), Some("shift+KeyD"));
        assert_eq!(saved.ptt_hotkey.as_deref(), Some("F9"));
    }

    #[test]
    fn hotkey_pressed_toggles_only_in_room_and_when_not_capturing() {
        let mut state = state_with_code();
        assert!(
            state
                .handle_action(UserAction::HotkeyPressed(HotkeyAction::ToggleMute))
                .is_empty()
        );
        assert!(
            state
                .handle_action(UserAction::HotkeyPressed(HotkeyAction::ToggleDeafen))
                .is_empty()
        );

        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::HotkeyPressed(HotkeyAction::ToggleMute)),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert!(room(&state).muted);

        state.handle_action(UserAction::StartHotkeyCapture(HotkeyAction::ToggleMute));
        assert!(
            state
                .handle_action(UserAction::HotkeyPressed(HotkeyAction::ToggleMute))
                .is_empty()
        );
        assert!(room(&state).muted);

        state.handle_action(UserAction::CancelHotkeyCapture);
        assert_eq!(
            state.handle_action(UserAction::HotkeyPressed(HotkeyAction::ToggleDeafen)),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened)
            ]
        );
        assert!(room(&state).deafened);
    }

    #[test]
    fn mute_and_deafen_cues_cover_all_transitions_and_skip_ignored_mute_and_join() {
        let mut state = state_with_code();
        state.form.muted_on_join = true;
        let login_cmds = state.handle_action(UserAction::SubmitLogin);
        assert!(
            !login_cmds
                .iter()
                .any(|c| matches!(c, UiCommand::PlayCue(_)))
        );
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            participants: vec![person("ana")],
        });
        assert!(room(&state).muted);

        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(false), UiCommand::PlayCue(Cue::Unmuted)]
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened)
            ]
        );
        assert!(state.handle_action(UserAction::ToggleMute).is_empty());
        assert!(
            state
                .handle_action(UserAction::HotkeyPressed(HotkeyAction::ToggleMute))
                .is_empty()
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
    }

    fn ptt_state_in_room() -> AppState {
        let mut settings = valid_settings();
        settings.voice_mode = VoiceMode::PushToTalk;
        settings.ptt_hotkey = Some("control+Space".to_owned());
        let mut state = AppState::new(&settings);
        state.form.invite_code = "segredo".to_owned();
        in_room(&mut state);
        state.take_pending_save();
        state
    }

    #[test]
    fn login_in_push_to_talk_sends_single_mute_and_starts_unmuted_in_ui() {
        let mut settings = valid_settings();
        settings.voice_mode = VoiceMode::PushToTalk;
        let mut state = AppState::new(&settings);
        state.form.invite_code = "segredo".to_owned();
        assert_eq!(
            state.handle_action(UserAction::SubmitLogin),
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            participants: vec![],
        });
        assert!(!room(&state).muted);
        assert!(room(&state).mic_sent);

        let mut both = AppState::new(&Settings {
            muted_on_join: true,
            ..settings
        });
        both.form.invite_code = "segredo".to_owned();
        assert_eq!(
            both.handle_action(UserAction::SubmitLogin),
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
        both.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            participants: vec![],
        });
        assert!(room(&both).muted);
        assert!(room(&both).mic_sent);
    }

    #[test]
    fn push_to_talk_opens_on_press_and_closes_after_release_delay() {
        let mut state = ptt_state_in_room();
        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Segure Ctrl+Espaço para falar")
        );

        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert!(room(&state).ptt_held);
        assert!(!room(&state).mic_sent);
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Falando (push-to-talk)")
        );
        assert!(state.push_to_talk(true, t0).is_empty());

        assert!(state.push_to_talk(false, t0).is_empty());
        assert!(!room(&state).ptt_held);
        assert_eq!(room(&state).ptt_release_at, Some(t0 + PTT_RELEASE_DELAY));
        assert!(!room(&state).mic_sent);
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Falando (push-to-talk)")
        );

        let (cmds, wait) = state.poll_push_to_talk(t0 + Duration::from_millis(50));
        assert!(cmds.is_empty());
        assert_eq!(wait, Some(Duration::from_millis(150)));
        assert!(!room(&state).mic_sent);

        let (cmds, wait) = state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY);
        assert_eq!(cmds, vec![UiCommand::SetMuted(true)]);
        assert_eq!(wait, None);
        assert_eq!(room(&state).ptt_release_at, None);
        assert!(room(&state).mic_sent);
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Segure Ctrl+Espaço para falar")
        );
        assert_eq!(
            state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY),
            (Vec::new(), None)
        );
    }

    #[test]
    fn repressing_during_release_delay_cancels_timer_without_extra_command() {
        let mut state = ptt_state_in_room();
        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert!(state.push_to_talk(false, t0).is_empty());
        assert!(
            state
                .push_to_talk(true, t0 + Duration::from_millis(100))
                .is_empty()
        );
        assert!(room(&state).ptt_held);
        assert_eq!(room(&state).ptt_release_at, None);
        assert_eq!(
            state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY),
            (Vec::new(), None)
        );
        assert!(!room(&state).mic_sent);
    }

    #[test]
    fn manual_mute_overrides_push_to_talk() {
        let mut state = ptt_state_in_room();
        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert!(room(&state).muted);
        assert!(room(&state).mic_sent);
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Segure Ctrl+Espaço para falar")
        );

        assert!(state.push_to_talk(false, t0).is_empty());
        assert_eq!(
            state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY),
            (Vec::new(), None)
        );
        assert!(state.push_to_talk(true, t0 + PTT_RELEASE_DELAY).is_empty());
        assert!(room(&state).mic_sent);

        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(false), UiCommand::PlayCue(Cue::Unmuted)]
        );
        assert!(!room(&state).muted);
        assert!(!room(&state).mic_sent);
    }

    #[test]
    fn deafen_during_push_to_talk_mutes_and_undeafen_respects_ptt_state() {
        let mut state = ptt_state_in_room();
        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened)
            ]
        );
        assert!(room(&state).deafened);
        assert!(room(&state).mic_sent);

        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(false),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
        assert!(!room(&state).mic_sent);

        state.handle_action(UserAction::ToggleDeafen);
        state.push_to_talk(false, t0);
        state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY);
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
        assert!(!room(&state).muted);
        assert!(room(&state).mic_sent);
    }

    #[test]
    fn switching_voice_mode_in_room_resets_ptt_and_syncs_mic() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.take_pending_save();
        assert_eq!(state.push_to_talk_hint(), None);

        assert_eq!(
            state.handle_action(UserAction::SelectVoiceMode(VoiceMode::PushToTalk)),
            vec![UiCommand::SetMuted(true)]
        );
        assert_eq!(
            state.take_pending_save().unwrap().voice_mode,
            VoiceMode::PushToTalk
        );
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Defina a tecla de push-to-talk nas Configurações")
        );
        assert!(
            state
                .handle_action(UserAction::SelectVoiceMode(VoiceMode::PushToTalk))
                .is_empty()
        );

        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert!(
            state
                .handle_action(UserAction::SelectVoiceMode(VoiceMode::VoiceActivity))
                .is_empty()
        );
        assert!(!room(&state).ptt_held);
        assert_eq!(room(&state).ptt_release_at, None);
        assert!(!room(&state).mic_sent);

        assert_eq!(
            state.handle_action(UserAction::SelectVoiceMode(VoiceMode::PushToTalk)),
            vec![UiCommand::SetMuted(true)]
        );
        assert_eq!(
            state.handle_action(UserAction::SelectVoiceMode(VoiceMode::VoiceActivity)),
            vec![UiCommand::SetMuted(false)]
        );
    }

    #[test]
    fn push_to_talk_outside_room_or_in_voice_activity_mode_does_nothing() {
        let t0 = Instant::now();
        let mut state = state_with_code();
        state.handle_action(UserAction::SelectVoiceMode(VoiceMode::PushToTalk));
        assert!(state.push_to_talk(true, t0).is_empty());
        assert!(state.push_to_talk(false, t0).is_empty());
        assert_eq!(state.poll_push_to_talk(t0), (Vec::new(), None));

        let mut va = state_with_code();
        in_room(&mut va);
        assert!(va.push_to_talk(true, t0).is_empty());
        assert!(va.push_to_talk(false, t0).is_empty());
        assert!(!room(&va).ptt_held);
        assert_eq!(va.poll_push_to_talk(t0), (Vec::new(), None));
    }
}
