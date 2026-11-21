//! Noise suppression, noise gate and microphone level reading.

use crate::platform::settings::NoiseMode;
use crate::state::AppState;
use crate::ui_model::UiCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicLevelView {
    pub level_db: i8,
    pub gate_open: bool,
}

impl AppState {
    pub(in crate::state) fn select_noise(&mut self, mode: NoiseMode) -> Vec<UiCommand> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{in_room, valid_settings};
    use crate::ui_model::UiEvent;

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
        state.handle_action(UserAction::SelectNoiseMode(NoiseMode::Off));
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
    fn noise_mode_is_sent_saved_and_restored_on_join() {
        let mut state = AppState::new(&valid_settings(), None);
        assert_eq!(
            state.handle_action(UserAction::SelectNoiseMode(NoiseMode::Standard)),
            vec![UiCommand::SetNoiseSuppression(NoiseMode::Standard)]
        );
        assert!(
            state
                .handle_action(UserAction::SelectNoiseMode(NoiseMode::Standard))
                .is_empty()
        );
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.noise_suppression, NoiseMode::Standard);

        let mut restored = AppState::new(&saved, Some("segredo".to_owned()));
        let commands = restored.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(commands.contains(&UiCommand::SetNoiseSuppression(NoiseMode::Standard)));
    }
}
