//! Seleção e listagem de dispositivos de entrada e saída de áudio.

use crate::state::AppState;
use crate::ui_model::{AudioDeviceView, UiCommand};

pub const DEFAULT_DEVICE_LABEL: &str = "Padrão do sistema";
pub const MISSING_DEVICE_LABEL: &str = "(indisponível)";
pub const LOADING_DEVICE_LABEL: &str = "Carregando...";

pub(super) fn selected_label(
    devices: &[AudioDeviceView],
    selected: Option<&str>,
    loaded: bool,
) -> String {
    let Some(id) = selected else {
        return DEFAULT_DEVICE_LABEL.to_owned();
    };
    match devices.iter().find(|device| device.id == id) {
        Some(device) => device.name.clone(),
        None if loaded => MISSING_DEVICE_LABEL.to_owned(),
        None => LOADING_DEVICE_LABEL.to_owned(),
    }
}

impl AppState {
    pub(in crate::state) fn select_input(&mut self, id: Option<String>) -> Vec<UiCommand> {
        if self.audio.input == id {
            return Vec::new();
        }
        self.audio.input = id.clone();
        self.pending_save = Some(self.settings_snapshot());
        vec![UiCommand::SetInputDevice(id)]
    }

    pub(in crate::state) fn select_output(&mut self, id: Option<String>) -> Vec<UiCommand> {
        if self.audio.output == id {
            return Vec::new();
        }
        self.audio.output = id.clone();
        self.pending_save = Some(self.settings_snapshot());
        vec![UiCommand::SetOutputDevice(id)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::settings::Settings;
    use crate::state::test_support::{
        devices_event, in_room, mic, room, state_with_code, valid_settings,
    };
    use crate::state::{Screen, UserAction};
    use crate::ui_model::UiEvent;

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
}
