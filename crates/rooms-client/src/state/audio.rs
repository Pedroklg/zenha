//! Estado de áudio, dispositivos, volumes por participante, atalhos e push-to-talk.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use rooms_core::Cue;

use super::{AppState, Screen};
use crate::hotkeys::{HotkeyAction, binding_label};
use crate::settings::{NoiseMode, Settings, VoiceMode};
use crate::ui_model::{AudioDeviceView, UiCommand};

pub const PTT_RELEASE_DELAY: Duration = Duration::from_millis(200);
pub const DEFAULT_DEVICE_LABEL: &str = "Padrão do sistema";
pub const MISSING_DEVICE_LABEL: &str = "(indisponível)";
pub const LOADING_DEVICE_LABEL: &str = "Carregando...";
pub const FULL_VOLUME: u8 = 100;
pub const DEFAULT_PARTICIPANT_VOLUME: u16 = 100;
pub const MAX_PARTICIPANT_VOLUME: u16 = 200;

pub fn clamp_participant_volume(percent: u16) -> u16 {
    percent.min(MAX_PARTICIPANT_VOLUME)
}

pub fn participant_volume_for(volumes: &BTreeMap<String, u16>, identity: &str) -> u16 {
    volumes
        .get(identity)
        .copied()
        .map(clamp_participant_volume)
        .unwrap_or(DEFAULT_PARTICIPANT_VOLUME)
}

/// Applies `percent` to `volumes`, omitting `100%`, and returns the new value when it changed.
pub fn update_participant_volume(
    volumes: &mut BTreeMap<String, u16>,
    identity: String,
    percent: u16,
) -> Option<u16> {
    if identity.trim().is_empty() {
        return None;
    }
    let clamped = clamp_participant_volume(percent);
    if clamped == DEFAULT_PARTICIPANT_VOLUME {
        volumes
            .remove(&identity)
            .map(|_| DEFAULT_PARTICIPANT_VOLUME)
    } else if volumes.get(&identity).copied() == Some(clamped) {
        None
    } else {
        volumes.insert(identity, clamped);
        Some(clamped)
    }
}

pub fn normalize_participant_volumes(raw: &BTreeMap<String, u16>) -> BTreeMap<String, u16> {
    let mut normalized = BTreeMap::new();
    for (identity, &percent) in raw {
        update_participant_volume(&mut normalized, identity.clone(), percent);
    }
    normalized
}

pub fn saved_volume_commands(volumes: &BTreeMap<String, u16>) -> Vec<UiCommand> {
    normalize_participant_volumes(volumes)
        .into_iter()
        .map(|(identity, percent)| UiCommand::SetParticipantVolume { identity, percent })
        .collect()
}

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
    pub participant_volumes: BTreeMap<String, u16>,
    pub noise_gate: Option<i8>,
    pub mic_level: Option<MicLevelView>,
    pub(super) loaded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicLevelView {
    pub level_db: i8,
    pub gate_open: bool,
}

impl AudioState {
    pub(super) fn from_settings(settings: &Settings) -> Self {
        Self {
            input: settings.input_device.clone(),
            output: settings.output_device.clone(),
            noise: settings.noise_suppression,
            voice_mode: settings.voice_mode,
            stream_volume: settings
                .stream_volume
                .unwrap_or(FULL_VOLUME)
                .min(FULL_VOLUME),
            participant_volumes: normalize_participant_volumes(&settings.participant_volumes),
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
    pub(super) fn from_settings(settings: &Settings) -> Self {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LobbyVoiceState {
    pub muted: bool,
    pub deafened: bool,
    pub(super) muted_before_deafen: bool,
}

impl LobbyVoiceState {
    pub(super) fn from_muted(muted: bool) -> Self {
        Self {
            muted,
            deafened: false,
            muted_before_deafen: muted,
        }
    }

    pub fn effective_muted(self, mode: VoiceMode) -> bool {
        self.muted || mode == VoiceMode::PushToTalk
    }
}

pub(super) fn toggle_mute_fields(
    muted: &mut bool,
    deafened: bool,
    mic_sent: &mut bool,
    ptt_active: bool,
    mode: VoiceMode,
) -> Vec<UiCommand> {
    if deafened {
        return Vec::new();
    }
    *muted = !*muted;
    let cue = if *muted { Cue::Muted } else { Cue::Unmuted };
    let effective = *muted || (mode == VoiceMode::PushToTalk && !ptt_active);
    let mut commands = Vec::new();
    if effective != *mic_sent {
        *mic_sent = effective;
        commands.push(UiCommand::SetMuted(effective));
    }
    commands.push(UiCommand::PlayCue(cue));
    commands
}

pub(super) fn toggle_deafen_fields(
    muted: &mut bool,
    deafened: &mut bool,
    muted_before_deafen: &mut bool,
    mic_sent: &mut bool,
    ptt_active: bool,
    mode: VoiceMode,
) -> Vec<UiCommand> {
    if *deafened {
        *deafened = false;
        *muted = *muted_before_deafen;
        let effective = *muted || (mode == VoiceMode::PushToTalk && !ptt_active);
        *mic_sent = effective;
        vec![
            UiCommand::SetDeafened(false),
            UiCommand::SetMuted(effective),
            UiCommand::PlayCue(Cue::Undeafened),
        ]
    } else {
        *muted_before_deafen = *muted;
        *deafened = true;
        *muted = true;
        *mic_sent = true;
        vec![
            UiCommand::SetDeafened(true),
            UiCommand::SetMuted(true),
            UiCommand::PlayCue(Cue::Deafened),
        ]
    }
}

impl AppState {
    pub fn set_hotkey_failures(&mut self, failed: Vec<HotkeyAction>) {
        self.hotkeys.failed = failed;
    }

    fn is_deafened(&self) -> bool {
        match &self.screen {
            Screen::InRoom(room) => room.deafened,
            Screen::Onboarding | Screen::Lobby | Screen::Connecting { .. } => {
                self.lobby_voice.deafened
            }
        }
    }

    pub(super) fn room_cue(&self, cue: Cue) -> Option<UiCommand> {
        (self.room_sounds && !self.is_deafened()).then_some(UiCommand::PlayCue(cue))
    }

    pub(super) fn self_left_cue(&self) -> Option<UiCommand> {
        matches!(self.screen, Screen::InRoom(_))
            .then(|| self.room_cue(Cue::SelfLeft))
            .flatten()
    }

    pub fn push_to_talk(&mut self, pressed: bool, now: Instant) -> Vec<UiCommand> {
        if self.audio.voice_mode != VoiceMode::PushToTalk {
            return Vec::new();
        }
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        let mode = self.audio.voice_mode;
        let commands: Vec<UiCommand> = if pressed {
            room.ptt_held = true;
            room.ptt_release_at = None;
            room.sync_mic(mode).into_iter().collect()
        } else {
            if room.ptt_held {
                room.ptt_held = false;
                room.ptt_release_at = Some(now + PTT_RELEASE_DELAY);
            }
            Vec::new()
        };
        self.session_mic_sent = room.mic_sent;
        commands
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
            self.session_mic_sent = room.mic_sent;
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

    pub(super) fn toggle_mute(&mut self) -> Vec<UiCommand> {
        let mode = self.audio.voice_mode;
        match &mut self.screen {
            Screen::InRoom(room) => {
                let commands = room.toggle_mute(mode);
                self.session_mic_sent = room.mic_sent;
                commands
            }
            Screen::Lobby => toggle_mute_fields(
                &mut self.lobby_voice.muted,
                self.lobby_voice.deafened,
                &mut self.session_mic_sent,
                false,
                mode,
            ),
            Screen::Onboarding | Screen::Connecting { .. } => Vec::new(),
        }
    }

    pub(super) fn toggle_deafen(&mut self) -> Vec<UiCommand> {
        let mode = self.audio.voice_mode;
        match &mut self.screen {
            Screen::InRoom(room) => {
                let commands = room.toggle_deafen(mode);
                self.session_mic_sent = room.mic_sent;
                self.session_deafened = room.deafened;
                commands
            }
            Screen::Lobby => {
                let mut mic_sent = self.session_mic_sent;
                let mut deafened = self.lobby_voice.deafened;
                let commands = toggle_deafen_fields(
                    &mut self.lobby_voice.muted,
                    &mut deafened,
                    &mut self.lobby_voice.muted_before_deafen,
                    &mut mic_sent,
                    false,
                    mode,
                );
                self.lobby_voice.deafened = deafened;
                self.session_mic_sent = mic_sent;
                self.session_deafened = deafened;
                commands
            }
            Screen::Onboarding | Screen::Connecting { .. } => Vec::new(),
        }
    }

    pub(super) fn select_input(&mut self, id: Option<String>) -> Vec<UiCommand> {
        if self.audio.input == id {
            return Vec::new();
        }
        self.audio.input = id.clone();
        self.pending_save = Some(self.settings_snapshot());
        vec![UiCommand::SetInputDevice(id)]
    }

    pub(super) fn select_output(&mut self, id: Option<String>) -> Vec<UiCommand> {
        if self.audio.output == id {
            return Vec::new();
        }
        self.audio.output = id.clone();
        self.pending_save = Some(self.settings_snapshot());
        vec![UiCommand::SetOutputDevice(id)]
    }

    pub(super) fn select_noise(&mut self, mode: NoiseMode) -> Vec<UiCommand> {
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

    pub(super) fn select_voice_mode(&mut self, mode: VoiceMode) -> Vec<UiCommand> {
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
        let commands = room.sync_mic(mode).into_iter().collect();
        self.session_mic_sent = room.mic_sent;
        commands
    }

    pub(super) fn set_hotkey(&mut self, action: HotkeyAction, binding: Option<String>) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{
        devices_event, in_room, mic, person, room, state_with_code, valid_settings,
    };
    use crate::ui_model::UiEvent;

    #[test]
    fn stream_volume_applies_while_moving_and_saves_when_asked() {
        let mut state = AppState::new(&valid_settings(), None);
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

        let mut restored = AppState::new(&saved, Some("segredo".to_owned()));
        assert_eq!(restored.screen, Screen::Lobby);
        let commands = restored.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(commands.contains(&UiCommand::SetStreamVolume(40)));
    }

    #[test]
    fn noise_gate_applies_without_saving_then_saves_and_restores_on_join() {
        let mut state = AppState::new(&valid_settings(), None);
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

        let mut restored = AppState::new(&saved, Some("segredo".to_owned()));
        let commands = restored.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(commands.contains(&UiCommand::SetNoiseGate(Some(-45))));

        let mut off = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        let commands = off.handle_action(UserAction::JoinRoom("geral".to_owned()));
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
        let mut state = AppState::new(&settings, None);
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
        let mut state = AppState::new(&valid_settings(), None);
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
    fn noise_mode_is_sent_saved_and_restored_on_join() {
        let mut state = AppState::new(&valid_settings(), None);
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

        let mut restored = AppState::new(&saved, Some("segredo".to_owned()));
        let commands = restored.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(commands.contains(&UiCommand::SetNoiseSuppression(NoiseMode::Enhanced)));
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
        assert_eq!(state.audio.output_label(), DEFAULT_DEVICE_LABEL);
        assert_eq!(
            state.handle_action(UserAction::SelectOutputDevice(Some("o1".to_owned()))),
            vec![UiCommand::SetOutputDevice(Some("o1".to_owned()))]
        );
        assert_eq!(
            state.take_pending_save().unwrap().output_device.as_deref(),
            Some("o1")
        );
        state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        assert_eq!(room(&state).room, "jogos");
        assert_eq!(state.audio.output.as_deref(), Some("o1"));
    }

    #[test]
    fn audio_devices_updated_replaces_the_lists_and_ignores_the_backend_selection() {
        let mut state = AppState::new(
            &Settings {
                input_device: Some("m1".to_owned()),
                ..valid_settings()
            },
            None,
        );
        state.apply_event(devices_event(&["m1", "m2"], &["o1"]));
        assert_eq!(state.audio.inputs, vec![mic("m1"), mic("m2")]);
        assert_eq!(state.audio.outputs, vec![mic("o1")]);
        state.apply_event(devices_event(&["m3"], &[]));
        assert_eq!(state.audio.inputs, vec![mic("m3")]);
        assert!(state.audio.outputs.is_empty());
        assert_eq!(state.audio.input.as_deref(), Some("m1"));
        assert_eq!(state.screen, Screen::Onboarding);
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
        let mut state = AppState::new(
            &Settings {
                input_device: Some("m1".to_owned()),
                ..valid_settings()
            },
            None,
        );
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
    fn first_join_configures_devices_once_before_mute_and_join_and_closes_settings() {
        let mut state = AppState::new(
            &Settings {
                input_device: Some("m1".to_owned()),
                output_device: Some("o1".to_owned()),
                muted_on_join: true,
                ..valid_settings()
            },
            None,
        );
        state.form.invite_code = "segredo".to_owned();
        state.handle_action(UserAction::OpenSettings);
        state.handle_action(UserAction::SubmitOnboarding);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.input_device.as_deref(), Some("m1"));
        assert_eq!(saved.output_device.as_deref(), Some("o1"));

        let commands = state.handle_action(UserAction::JoinRoom("geral".to_owned()));
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

        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        state.handle_action(UserAction::Leave);

        // Second room join must not repeat device/audio setup commands.
        let second_commands = state.handle_action(UserAction::JoinRoom("jogos".to_owned()));
        assert_eq!(
            second_commands,
            vec![UiCommand::Join {
                room: "jogos".to_owned()
            }]
        );
    }

    #[test]
    fn saved_device_choices_load_from_settings() {
        let state = AppState::new(
            &Settings {
                input_device: Some("m1".to_owned()),
                ..valid_settings()
            },
            None,
        );
        assert_eq!(state.audio.input.as_deref(), Some("m1"));
        assert_eq!(state.audio.output, None);
    }

    #[test]
    fn hotkeys_load_save_and_suspend_registrations_while_capturing() {
        let mut settings = valid_settings();
        settings.mute_hotkey = Some("control+KeyM".to_owned());
        settings.deafen_hotkey = Some("control+KeyD".to_owned());
        settings.ptt_hotkey = Some("F9".to_owned());
        let mut state = AppState::new(&settings, None);
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
        let mut state = AppState::new(&valid_settings(), None);
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
        state.muted_on_join = true;
        let onboarding_cmds = state.handle_action(UserAction::SubmitOnboarding);
        assert!(
            !onboarding_cmds
                .iter()
                .any(|c| matches!(c, UiCommand::PlayCue(_)))
        );
        let join_cmds = state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(!join_cmds.iter().any(|c| matches!(c, UiCommand::PlayCue(_))));
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
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
        let mut state = AppState::new(&settings, None);
        state.form.invite_code = "segredo".to_owned();
        in_room(&mut state);
        state.take_pending_save();
        state
    }

    #[test]
    fn join_from_lobby_in_push_to_talk_sends_single_mute_and_starts_unmuted_in_ui() {
        let mut settings = valid_settings();
        settings.voice_mode = VoiceMode::PushToTalk;
        let mut state = AppState::new(&settings, Some("segredo".to_owned()));
        assert_eq!(state.screen, Screen::Lobby);
        assert_eq!(
            state.handle_action(UserAction::JoinRoom("geral".to_owned())),
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        assert!(!room(&state).muted);
        assert!(room(&state).mic_sent);

        let mut both = AppState::new(
            &Settings {
                muted_on_join: true,
                ..settings
            },
            Some("segredo".to_owned()),
        );
        assert_eq!(
            both.handle_action(UserAction::JoinRoom("geral".to_owned())),
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
        both.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
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

    #[test]
    fn lobby_voice_state_carries_across_rooms_and_back_on_leave() {
        let mut state = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert!(state.lobby_voice.muted);

        assert_eq!(
            state.handle_action(UserAction::JoinRoom("geral".to_owned())),
            vec![UiCommand::Join {
                room: "geral".to_owned()
            }]
        );
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        assert!(room(&state).muted);

        state.handle_action(UserAction::ToggleDeafen);
        state.handle_action(UserAction::Leave);
        assert_eq!(state.screen, Screen::Lobby);
        assert!(state.lobby_voice.muted);
        assert!(state.lobby_voice.deafened);

        state.handle_action(UserAction::JoinRoom("jogos".to_owned()));
        state.apply_event(UiEvent::Joined {
            room: "jogos".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        assert!(room(&state).deafened);
        assert!(room(&state).muted);
    }

    #[test]
    fn muted_on_join_setting_updates_and_saves() {
        let mut state = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        assert!(!state.muted_on_join);
        state.handle_action(UserAction::SetMutedOnJoin(true));
        assert!(state.muted_on_join);
        assert!(state.lobby_voice.muted);
        assert!(state.take_pending_save().unwrap().muted_on_join);
        assert_eq!(
            state.handle_action(UserAction::JoinRoom("geral".to_owned())),
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
    }

    #[test]
    fn participant_volume_helpers_clamp_normalize_and_emit_commands() {
        assert_eq!(clamp_participant_volume(0), 0);
        assert_eq!(clamp_participant_volume(140), 140);
        assert_eq!(clamp_participant_volume(260), 200);

        let raw = BTreeMap::from([
            ("alice".to_owned(), 70),
            ("bob".to_owned(), 100),
            ("carol".to_owned(), 250),
            ("   ".to_owned(), 40),
        ]);
        let normalized = normalize_participant_volumes(&raw);
        assert_eq!(
            normalized,
            BTreeMap::from([("alice".to_owned(), 70), ("carol".to_owned(), 200)])
        );
        assert_eq!(participant_volume_for(&normalized, "alice"), 70);
        assert_eq!(participant_volume_for(&normalized, "bob"), 100);
        assert_eq!(participant_volume_for(&normalized, "carol"), 200);
        assert_eq!(
            saved_volume_commands(&raw),
            vec![
                UiCommand::SetParticipantVolume {
                    identity: "alice".to_owned(),
                    percent: 70,
                },
                UiCommand::SetParticipantVolume {
                    identity: "carol".to_owned(),
                    percent: 200,
                },
            ]
        );
    }

    #[test]
    fn participant_volume_updates_live_without_saving_removes_100_and_sends_on_startup_and_join() {
        let mut state = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        assert!(state.take_pending_commands().is_empty());

        // Setting 100 when absent is a no-op.
        assert!(
            state
                .handle_action(UserAction::SetParticipantVolume {
                    identity: "alice".to_owned(),
                    percent: 100,
                })
                .is_empty()
        );

        // Adjusting clamps above 200 and sends to core without triggering a settings save.
        assert_eq!(
            state.handle_action(UserAction::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 250,
            }),
            vec![UiCommand::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 200,
            }]
        );
        assert!(state.take_pending_save().is_none());
        assert_eq!(
            state.handle_action(UserAction::SetParticipantVolume {
                identity: "bob".to_owned(),
                percent: 65,
            }),
            vec![UiCommand::SetParticipantVolume {
                identity: "bob".to_owned(),
                percent: 65,
            }]
        );
        // Repeating the same value does not spam commands.
        assert!(
            state
                .handle_action(UserAction::SetParticipantVolume {
                    identity: "bob".to_owned(),
                    percent: 65,
                })
                .is_empty()
        );

        // Resetting bob to 100 sends 100 to core and removes the entry before saving.
        assert_eq!(
            state.handle_action(UserAction::SetParticipantVolume {
                identity: "bob".to_owned(),
                percent: 100,
            }),
            vec![UiCommand::SetParticipantVolume {
                identity: "bob".to_owned(),
                percent: 100,
            }]
        );
        assert!(state.take_pending_save().is_none());

        state.handle_action(UserAction::SaveAudioSettings);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(
            saved.participant_volumes,
            BTreeMap::from([("alice".to_owned(), 200)])
        );

        // Next app startup queues SetParticipantVolume immediately and also sends on first join.
        let mut restarted =
            AppState::new_with_build_default(&saved, Some("segredo".to_owned()), None);
        assert_eq!(restarted.screen, Screen::Lobby);
        assert_eq!(
            restarted.take_pending_commands(),
            vec![UiCommand::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 200,
            }]
        );
        let join_cmds = restarted.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(join_cmds.contains(&UiCommand::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 200,
        }));
    }

    #[test]
    fn room_sounds_play_for_join_switch_leave_and_peer_changes_not_on_reconnect() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(state.take_pending_commands().is_empty());

        // Entering the room with peers already present only plays SelfJoined (no PeerJoined).
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("ana")],
        });
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::SelfJoined)]
        );

        // Another participant joining/leaving while connected emits PeerJoined / PeerLeft.
        state.apply_event(UiEvent::ParticipantJoined(person("bruno")));
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::PeerJoined)]
        );
        // Joining event for local user or duplicate update must not emit PeerJoined.
        state.apply_event(UiEvent::ParticipantJoined(person("pedro")));
        state.apply_event(UiEvent::ParticipantUpdated(person("bruno")));
        assert!(state.take_pending_commands().is_empty());

        state.apply_event(UiEvent::ParticipantLeft {
            identity: "bruno".to_owned(),
        });
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::PeerLeft)]
        );

        // Reconnect and subsequent ParticipantList/Joined must not emit any join/leave cue.
        state.apply_event(UiEvent::Reconnecting);
        state.apply_event(UiEvent::ParticipantJoined(person("carla")));
        state.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(state.take_pending_commands().is_empty());
        state.apply_event(UiEvent::Reconnected);
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("carla")],
        });
        assert!(state.take_pending_commands().is_empty());

        // Room switch emits SelfLeft immediately, then SelfJoined once the new room connects.
        let switch_cmds = state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        assert_eq!(
            switch_cmds,
            vec![
                UiCommand::PlayCue(Cue::SelfLeft),
                UiCommand::Join {
                    room: "jogos".to_owned(),
                },
            ]
        );
        state.apply_event(UiEvent::Joined {
            room: "jogos".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("ana")],
        });
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::SelfJoined)]
        );

        // CancelConnect from Lobby->Connecting must not emit SelfLeft.
        assert_eq!(
            state.handle_action(UserAction::Leave),
            vec![UiCommand::PlayCue(Cue::SelfLeft), UiCommand::Leave]
        );
        state.handle_action(UserAction::JoinRoom("musica".to_owned()));
        assert_eq!(
            state.handle_action(UserAction::CancelConnect),
            vec![UiCommand::Leave]
        );
    }

    #[test]
    fn room_sounds_are_suppressed_when_deafened_or_disabled_but_mute_cues_remain() {
        // Suppressed when deafened.
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::ToggleDeafen);
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("ana")],
        });
        state.apply_event(UiEvent::ParticipantJoined(person("bruno")));
        state.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(state.take_pending_commands().is_empty());
        assert_eq!(
            state.handle_action(UserAction::SwitchRoom("jogos".to_owned())),
            vec![UiCommand::Join {
                room: "jogos".to_owned(),
            }]
        );
        assert_eq!(
            state.handle_action(UserAction::Leave),
            vec![UiCommand::Leave]
        );
        // Un-deafening still plays Undeafened cue.
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(false),
                UiCommand::PlayCue(Cue::Undeafened),
            ]
        );

        // Suppressed when room_sounds setting is disabled, and setting persists.
        assert!(
            state
                .handle_action(UserAction::SetRoomSounds(false))
                .is_empty()
        );
        assert!(!state.room_sounds);
        let saved = state.take_pending_save().unwrap();
        assert!(!saved.room_sounds);

        let mut reloaded = AppState::new(&saved, Some("segredo".to_owned()));
        assert!(!reloaded.room_sounds);
        reloaded.handle_action(UserAction::JoinRoom("geral".to_owned()));
        reloaded.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro")],
        });
        reloaded.apply_event(UiEvent::ParticipantJoined(person("ana")));
        reloaded.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(reloaded.take_pending_commands().is_empty());

        // Mute / unmute and deafen cues still work when room_sounds is false.
        assert_eq!(
            reloaded.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert_eq!(
            reloaded.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened),
            ]
        );
        assert_eq!(
            reloaded.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Undeafened),
            ]
        );
        assert_eq!(
            reloaded.handle_action(UserAction::Leave),
            vec![UiCommand::Leave]
        );
    }
}
