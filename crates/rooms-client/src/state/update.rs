//! Estado do aviso de atualização disponível.

use super::AppState;
use crate::update::{AvailableUpdate, should_show_update};

impl AppState {
    pub fn set_available_update(&mut self, update: AvailableUpdate) {
        self.available_update = Some(update);
    }

    pub fn visible_update(&self) -> Option<&AvailableUpdate> {
        self.available_update
            .as_ref()
            .filter(|update| should_show_update(update, self.dismissed_update_version.as_deref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::valid_settings;

    #[test]
    fn available_update_dismisses_and_reappears_for_newer_version() {
        let mut state =
            AppState::new_with_build_default(&valid_settings(), Some("segredo".to_owned()), None);
        assert_eq!(state.visible_update(), None);

        let v2 = AvailableUpdate {
            version: "0.2.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0".to_owned(),
        };
        state.set_available_update(v2.clone());
        assert_eq!(state.visible_update(), Some(&v2));

        state.handle_action(UserAction::DismissUpdate);
        assert_eq!(state.visible_update(), None);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.dismissed_update_version.as_deref(), Some("0.2.0"));

        let mut reloaded =
            AppState::new_with_build_default(&saved, Some("segredo".to_owned()), None);
        reloaded.set_available_update(v2);
        assert_eq!(reloaded.visible_update(), None);

        let v3 = AvailableUpdate {
            version: "0.3.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.3.0".to_owned(),
        };
        reloaded.set_available_update(v3.clone());
        assert_eq!(reloaded.visible_update(), Some(&v3));
    }
}
