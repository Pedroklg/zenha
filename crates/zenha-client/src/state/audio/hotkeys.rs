//! State and configuration of global keyboard shortcuts.

use crate::platform::hotkeys::HotkeyAction;
use crate::platform::settings::Settings;
use crate::state::AppState;

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
    pub(in crate::state) fn from_settings(settings: &Settings) -> Self {
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

impl AppState {
    pub fn set_hotkey_failures(&mut self, failed: Vec<HotkeyAction>) {
        self.hotkeys.failed = failed;
    }

    pub(in crate::state) fn set_hotkey(&mut self, action: HotkeyAction, binding: Option<String>) {
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
    use zenha_core::Cue;

    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{in_room, room, state_with_code, valid_settings};
    use crate::ui_model::UiCommand;

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
            Some("Esse atalho já está em uso em \"Ligar/desligar microfone\".")
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
            Some("Esse atalho já está em uso em \"Ligar/desligar áudio\".")
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
}
