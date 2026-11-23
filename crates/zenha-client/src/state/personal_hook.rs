//! Personal Gamers Club webhook URL state: loading, copying, retrying and error handling.

use std::fmt;

use super::room_lock::SecretText;
use super::{AppState, Screen};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PersonalHookPhase {
    #[default]
    Idle,
    Loading,
    Ready(SecretText),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PersonalHookState {
    pub phase: PersonalHookPhase,
    pub copied: bool,
    request_seq: u64,
    active_request_id: Option<u64>,
    loaded_server: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PersonalHookFetch {
    pub id: u64,
    pub server_url: String,
    pub client_id: String,
    pub invite_code: String,
}

impl fmt::Debug for PersonalHookFetch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersonalHookFetch")
            .field("id", &self.id)
            .field("server_url", &self.server_url)
            .field("client_id", &self.client_id)
            .field("invite_code", &"<redacted>")
            .finish()
    }
}

impl PersonalHookState {
    pub fn url(&self) -> Option<&str> {
        match &self.phase {
            PersonalHookPhase::Ready(url) => Some(&url.0),
            _ => None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.phase = PersonalHookPhase::Idle;
        self.copied = false;
        self.active_request_id = None;
        self.loaded_server = None;
    }
}

impl AppState {
    pub fn take_personal_hook_fetch(&mut self) -> Option<PersonalHookFetch> {
        self.pending_personal_hook_fetch.take()
    }

    pub fn personal_hook_url(&self) -> Option<&str> {
        self.personal_hook.url()
    }

    pub(super) fn request_personal_hook(&mut self, force: bool) {
        if matches!(self.screen, Screen::Onboarding) {
            return;
        }
        let server_url = self.form.server_url.trim().to_owned();
        if server_url.is_empty() {
            return;
        }
        let same_server = self.personal_hook.loaded_server.as_deref() == Some(server_url.as_str());
        if !force
            && same_server
            && matches!(
                self.personal_hook.phase,
                PersonalHookPhase::Loading | PersonalHookPhase::Ready(_)
            )
        {
            return;
        }
        self.personal_hook.request_seq += 1;
        let id = self.personal_hook.request_seq;
        self.personal_hook.active_request_id = Some(id);
        self.personal_hook.loaded_server = Some(server_url.clone());
        self.personal_hook.phase = PersonalHookPhase::Loading;
        self.personal_hook.copied = false;
        self.pending_personal_hook_fetch = Some(PersonalHookFetch {
            id,
            server_url,
            client_id: self.client_id.clone(),
            invite_code: self.current_password(),
        });
    }

    pub(super) fn copy_personal_hook(&mut self) {
        if self.personal_hook.url().is_some() {
            self.personal_hook.copied = true;
        }
    }

    pub fn apply_personal_hook_result(&mut self, id: u64, result: Result<String, String>) {
        if self.personal_hook.active_request_id != Some(id) {
            return;
        }
        self.personal_hook.active_request_id = None;
        self.personal_hook.phase = match result {
            Ok(url) => PersonalHookPhase::Ready(SecretText(url)),
            Err(message) => PersonalHookPhase::Failed(message),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::state_with_code;
    use crate::ui_model::UiEvent;

    const SAMPLE_URL: &str =
        "https://voz.exemplo/hooks/u/client-1/0123456789abcdef0123456789abcdef";

    fn lobby_state() -> AppState {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state
    }

    #[test]
    fn opening_settings_loads_once_per_server_and_copy_sets_feedback() {
        let mut state = lobby_state();
        state.handle_action(UserAction::OpenSettings);
        assert_eq!(state.personal_hook.phase, PersonalHookPhase::Loading);

        let fetch = state.take_personal_hook_fetch().unwrap();
        assert_eq!(fetch.server_url, "http://localhost:8080");
        assert!(!format!("{fetch:?}").contains(&fetch.invite_code));

        // Copy before ready does nothing.
        state.handle_action(UserAction::CopyPersonalHook);
        assert!(!state.personal_hook.copied);

        state.apply_personal_hook_result(fetch.id, Ok(SAMPLE_URL.to_owned()));
        assert_eq!(state.personal_hook_url(), Some(SAMPLE_URL));
        assert!(!format!("{:?}", state.personal_hook).contains("0123456789abcdef"));

        state.handle_action(UserAction::CopyPersonalHook);
        assert!(state.personal_hook.copied);

        // Closing settings clears the "copied" hint, but keeps the URL cached.
        state.handle_action(UserAction::CloseSettings);
        assert!(!state.personal_hook.copied);
        assert_eq!(state.personal_hook_url(), Some(SAMPLE_URL));

        // Reopening settings on the same server does not refetch.
        state.handle_action(UserAction::OpenSettings);
        assert!(state.take_personal_hook_fetch().is_none());
        assert_eq!(state.personal_hook_url(), Some(SAMPLE_URL));
    }

    #[test]
    fn failure_shows_error_and_retry_requests_again_ignoring_stale_results() {
        let mut state = lobby_state();
        state.handle_action(UserAction::OpenSettings);
        let first = state.take_personal_hook_fetch().unwrap();

        state.apply_personal_hook_result(
            first.id,
            Err("Não foi possível falar com o servidor.".to_owned()),
        );
        assert_eq!(
            state.personal_hook.phase,
            PersonalHookPhase::Failed("Não foi possível falar com o servidor.".to_owned())
        );
        assert_eq!(state.personal_hook_url(), None);

        state.handle_action(UserAction::RetryPersonalHook);
        assert_eq!(state.personal_hook.phase, PersonalHookPhase::Loading);
        let second = state.take_personal_hook_fetch().unwrap();
        assert_ne!(second.id, first.id);

        // Stale response from first attempt is ignored.
        state.apply_personal_hook_result(
            first.id,
            Ok("https://stale.example/hooks/u/x/y".to_owned()),
        );
        assert_eq!(state.personal_hook.phase, PersonalHookPhase::Loading);

        state.apply_personal_hook_result(second.id, Ok(SAMPLE_URL.to_owned()));
        assert_eq!(state.personal_hook_url(), Some(SAMPLE_URL));
    }

    #[test]
    fn changing_server_or_invite_rejection_resets_personal_hook_and_drops_in_flight() {
        let mut state = lobby_state();
        state.handle_action(UserAction::OpenSettings);
        let fetch = state.take_personal_hook_fetch().unwrap();

        state.handle_action(UserAction::ChangeServer);
        assert_eq!(state.personal_hook.phase, PersonalHookPhase::Idle);
        state.apply_personal_hook_result(fetch.id, Ok(SAMPLE_URL.to_owned()));
        assert_eq!(state.personal_hook.phase, PersonalHookPhase::Idle);

        // Onboarding settings does not trigger a fetch.
        state.handle_action(UserAction::OpenSettings);
        assert!(state.take_personal_hook_fetch().is_none());

        state.handle_action(UserAction::CloseSettings);
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::OpenSettings);
        let fetch2 = state.take_personal_hook_fetch().unwrap();
        state.apply_personal_hook_result(fetch2.id, Ok(SAMPLE_URL.to_owned()));
        assert_eq!(state.personal_hook_url(), Some(SAMPLE_URL));

        state.apply_event(UiEvent::InviteRejected);
        assert_eq!(state.personal_hook.phase, PersonalHookPhase::Idle);
    }
}
