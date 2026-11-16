//! Estado de entrada no servidor, formulário de login e credenciais.

use std::fmt;

use rooms_protocol::{MAX_DISPLAY_NAME_LEN, ValidationError, validate_display_name};

use super::audio::{LobbyVoiceState, saved_volume_commands};
use super::{AppState, Screen};
use crate::cli::StartupLink;
use crate::settings::Settings;
use crate::ui_model::{Credentials, UiCommand};

#[derive(Clone, PartialEq, Eq, Default)]
pub struct LoginForm {
    pub display_name: String,
    pub server_url: String,
    pub invite_code: String,
}

impl fmt::Debug for LoginForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginForm")
            .field("display_name", &self.display_name)
            .field("server_url", &self.server_url)
            .field("invite_code", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginErrors {
    pub display_name: Option<String>,
    pub server_url: Option<String>,
}

impl LoginErrors {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none() && self.server_url.is_none()
    }
}

impl LoginForm {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            display_name: settings.display_name.clone(),
            server_url: settings.token_server_url.clone(),
            invite_code: String::new(),
        }
    }

    pub fn to_settings(&self) -> Settings {
        Settings {
            display_name: self.display_name.trim().to_owned(),
            token_server_url: self.server_url.trim().to_owned(),
            ..Settings::default()
        }
    }

    pub fn errors(&self) -> LoginErrors {
        LoginErrors {
            display_name: display_name_error(&self.display_name),
            server_url: server_url_error(&self.server_url),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.errors().is_empty()
    }
}

pub fn display_name_error(name: &str) -> Option<String> {
    validate_display_name(name).err().map(display_name_message)
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

fn server_url_error(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return Some("Informe o endereço do servidor.".to_owned());
    }
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"));
    match rest {
        Some(rest)
            if !rest.is_empty()
                && !rest.starts_with('/')
                && !url.chars().any(char::is_whitespace) =>
        {
            None
        }
        _ => Some("O endereço deve começar com http:// ou https:// e não ter espaços.".to_owned()),
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct OnboardingState {
    pub link_open: bool,
    pub link_input: String,
    pub link_error: Option<String>,
    pub password_rejected: bool,
    pub(super) link_pasted_server: Option<String>,
}

impl fmt::Debug for OnboardingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnboardingState")
            .field("link_open", &self.link_open)
            .field("link_input", &"<redacted>")
            .field("link_error", &self.link_error)
            .field("password_rejected", &self.password_rejected)
            .field("link_pasted_server", &self.link_pasted_server)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum CredentialOp {
    Store { server: String, password: String },
    Forget { server: String },
}

impl fmt::Debug for CredentialOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store { server, .. } => f
                .debug_struct("Store")
                .field("server", server)
                .field("password", &"<redacted>")
                .finish(),
            Self::Forget { server } => f.debug_struct("Forget").field("server", server).finish(),
        }
    }
}

impl AppState {
    pub fn apply_startup_link(&mut self, link: StartupLink) {
        match link {
            StartupLink::None => {}
            StartupLink::Invite(invite) => {
                self.discard_pending_paste();
                self.screen = Screen::Onboarding;
                self.active_password.clear();
                self.pending_password_save = None;
                self.apply_invite(invite);
                if self.can_submit_onboarding() {
                    let commands = self.submit_onboarding();
                    self.pending_commands.extend(commands);
                }
            }
            StartupLink::Invalid(raw) => match self.screen {
                Screen::Onboarding => {
                    self.onboarding.link_open = !raw.trim().is_empty();
                    self.paste_invite_link(raw);
                }
                Screen::Lobby => {
                    if !raw.trim().is_empty() {
                        self.lobby_error = Some("Link de convite inválido.".to_owned());
                    }
                }
                Screen::Connecting { .. } | Screen::InRoom(_) => {}
            },
        }
    }

    pub fn show_password_field(&self) -> bool {
        if self.onboarding.password_rejected {
            return true;
        }
        let server = self.form.server_url.trim();
        let has_saved = self
            .saved_credential
            .as_ref()
            .is_some_and(|(saved_server, _)| saved_server == server);
        let has_pasted = self.onboarding.link_pasted_server.as_deref() == Some(server);
        !has_saved && !has_pasted
    }

    fn resolved_password(&self) -> String {
        if !self.form.invite_code.is_empty() {
            return self.form.invite_code.clone();
        }
        let server = self.form.server_url.trim();
        if let Some((saved_server, saved_password)) = &self.saved_credential
            && saved_server == server
        {
            return saved_password.clone();
        }
        String::new()
    }

    pub(super) fn current_password(&self) -> String {
        if !matches!(self.screen, Screen::Onboarding) && !self.active_password.is_empty() {
            self.active_password.clone()
        } else {
            self.resolved_password()
        }
    }

    pub fn invite_link(&self) -> String {
        crate::invite::format(self.form.server_url.trim(), &self.current_password())
    }

    pub fn credentials(&self) -> Credentials {
        Credentials {
            token_server_url: self.form.server_url.trim().to_owned(),
            client_id: self.client_id.clone(),
            display_name: self.form.display_name.trim().to_owned(),
            invite_code: self.current_password(),
        }
    }

    pub fn can_save_name(&self) -> bool {
        self.name_draft.trim() != self.form.display_name.trim()
            && display_name_error(&self.name_draft).is_none()
    }

    pub(super) fn save_display_name(&mut self) -> Vec<UiCommand> {
        if !self.can_save_name() {
            return Vec::new();
        }
        let name = self.name_draft.trim().to_owned();
        self.name_draft.clone_from(&name);
        self.form.display_name.clone_from(&name);
        self.pending_save = Some(self.settings_snapshot());
        match &mut self.screen {
            Screen::Onboarding => return Vec::new(),
            Screen::InRoom(room) => {
                if let Some(me) = room
                    .participants
                    .iter_mut()
                    .find(|p| p.identity == room.local_identity)
                {
                    me.name.clone_from(&name);
                }
            }
            Screen::Lobby | Screen::Connecting { .. } => {}
        }
        vec![UiCommand::SetDisplayName { name }]
    }

    pub fn can_submit_onboarding(&self) -> bool {
        matches!(self.screen, Screen::Onboarding) && self.form.is_valid()
    }

    fn apply_invite(&mut self, invite: crate::invite::Invite) {
        self.form.server_url = invite.server.clone();
        self.form.invite_code = invite.password.clone();
        if invite.password.is_empty() {
            self.onboarding.link_pasted_server = None;
        } else {
            self.onboarding.link_pasted_server = Some(invite.server);
            self.onboarding.password_rejected = false;
        }
        self.onboarding.link_open = false;
        self.onboarding.link_input.clear();
        self.onboarding.link_error = None;
    }

    pub(super) fn paste_invite_link(&mut self, text: String) {
        if let Some(invite) = crate::invite::parse(&text) {
            self.apply_invite(invite);
        } else {
            self.onboarding.link_input = text;
            self.onboarding.link_error = (!self.onboarding.link_input.trim().is_empty())
                .then(|| "Link de convite inválido.".to_owned());
        }
    }

    pub(super) fn submit_onboarding(&mut self) -> Vec<UiCommand> {
        if !self.can_submit_onboarding() {
            return Vec::new();
        }
        let server = self.form.server_url.trim().to_owned();
        let password = self.resolved_password();
        self.active_password = password.clone();
        self.pending_password_save = (!password.is_empty()
            && self.saved_credential.as_ref() != Some(&(server.clone(), password.clone())))
        .then_some((server, password));
        self.pending_save = Some(self.settings_snapshot());
        self.lobby_voice = LobbyVoiceState::from_muted(self.muted_on_join);
        self.lobby_error = None;
        self.lobby_notice = None;
        self.pending_move_notice = None;
        self.rooms.clear();
        self.audio.open = false;
        self.admin.reset();
        self.invite_copied = false;
        self.audio_configured = false;
        self.session_mic_sent = false;
        self.session_deafened = false;
        self.onboarding.password_rejected = false;
        self.onboarding.link_open = false;
        self.onboarding.link_error = None;
        self.last_refresh = None;
        self.screen = Screen::Lobby;
        let mut commands = vec![UiCommand::RefreshRooms];
        commands.extend(saved_volume_commands(&self.audio.participant_volumes));
        commands
    }

    pub(super) fn change_server(&mut self) -> Vec<UiCommand> {
        let was_in_room = matches!(self.screen, Screen::InRoom(_) | Screen::Connecting { .. });
        let leave_cue = self.self_left_cue();
        if let Screen::InRoom(room) = &self.screen {
            self.lobby_voice = room.voice_state();
            self.session_mic_sent = room.mic_sent;
            self.session_deafened = room.deafened;
        }
        self.picker.close();
        self.audio.open = false;
        self.audio.mic_level = None;
        self.hotkeys.capturing = None;
        self.hotkeys.capture_error = None;
        self.lobby_error = None;
        self.lobby_notice = None;
        self.pending_move_notice = None;
        self.rooms.clear();
        self.admin.reset();
        self.onboarding.password_rejected = false;
        self.onboarding.link_open = false;
        self.onboarding.link_input.clear();
        self.onboarding.link_error = None;
        self.invite_copied = false;
        self.discard_pending_paste();
        self.screen = Screen::Onboarding;
        if was_in_room {
            leave_cue.into_iter().chain([UiCommand::Leave]).collect()
        } else {
            Vec::new()
        }
    }

    pub(super) fn on_invite_rejected(&mut self) {
        self.picker.close();
        self.audio.open = false;
        self.audio.mic_level = None;
        if matches!(self.screen, Screen::Onboarding) {
            return;
        }
        if matches!(self.screen, Screen::InRoom(_) | Screen::Connecting { .. }) {
            self.pending_commands.extend(self.self_left_cue());
            self.pending_commands.push(UiCommand::Leave);
        }
        self.rooms.clear();
        self.lobby_error = None;
        self.lobby_notice = None;
        self.pending_move_notice = None;
        self.admin.reset();
        self.pending_password_save = None;
        self.saved_credential = None;
        self.active_password.clear();
        self.form.invite_code.clear();
        self.onboarding.password_rejected = true;
        self.onboarding.link_pasted_server = None;
        self.onboarding.link_error = None;
        self.invite_copied = false;
        let server = self.form.server_url.trim().to_owned();
        if !server.is_empty() {
            self.pending_credential_ops
                .push(CredentialOp::Forget { server });
        }
        self.discard_pending_paste();
        self.screen = Screen::Onboarding;
    }
}

#[cfg(test)]
mod tests {
    use rooms_core::Cue;

    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{in_room, person, room, state_with_code, valid_settings};
    use crate::ui_model::UiEvent;

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
        assert!(form.errors().server_url.is_none());

        let mut form = state_with_code().form;
        form.display_name = "a".repeat(33);
        assert!(form.errors().display_name.is_some());

        // LoginForm::errors validates only display_name and server_url (no room or old code).
        let mut form = state_with_code().form;
        form.invite_code = String::new();
        assert!(form.is_valid());

        for bad in [
            "",
            "localhost:8080",
            "ftp://x",
            "http://",
            "http:///x",
            "http://a b",
        ] {
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
        let mut state = AppState::new(&valid_settings(), None);
        state.form.display_name = String::new();
        assert!(state.handle_action(UserAction::SubmitOnboarding).is_empty());
        assert_eq!(state.screen, Screen::Onboarding);
        assert!(state.take_pending_save().is_none());
    }

    #[test]
    fn startup_with_saved_data_and_password_opens_lobby_without_auto_joining_last_room() {
        let state = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        assert_eq!(state.screen, Screen::Lobby);
        assert_eq!(state.credentials().invite_code, "segredo");
        assert!(!state.show_password_field());
    }

    #[test]
    fn startup_without_saved_password_or_with_invalid_settings_opens_onboarding() {
        let no_pwd = AppState::new(&valid_settings(), None);
        assert_eq!(no_pwd.screen, Screen::Onboarding);
        assert!(no_pwd.show_password_field());

        let no_name = AppState::new(
            &Settings {
                display_name: String::new(),
                ..valid_settings()
            },
            Some("segredo".to_owned()),
        );
        assert_eq!(no_name.screen, Screen::Onboarding);
        assert!(!no_name.show_password_field());
    }

    #[test]
    fn rejected_password_returns_to_onboarding_shows_password_field_and_forgets_credential() {
        let mut state = AppState::new(&valid_settings(), Some("antiga".to_owned()));
        assert_eq!(state.screen, Screen::Lobby);

        // Reject on RefreshRooms in Lobby.
        state.apply_event(UiEvent::InviteRejected);
        assert_eq!(state.screen, Screen::Onboarding);
        assert!(state.onboarding.password_rejected);
        assert!(state.show_password_field());
        assert_eq!(
            state.take_credential_ops(),
            vec![CredentialOp::Forget {
                server: "http://localhost:8080".to_owned()
            }]
        );
        assert!(state.take_pending_commands().is_empty());

        // Enter wrong password and get rejected while joining a room; must NOT save on submit
        // and must leave cleanly + forget on rejection.
        state.form.invite_code = "errada".to_owned();
        state.handle_action(UserAction::SubmitOnboarding);
        assert!(state.take_credential_ops().is_empty());
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        state.apply_event(UiEvent::InviteRejected);
        assert_eq!(state.screen, Screen::Onboarding);
        assert!(state.show_password_field());
        assert_eq!(state.take_pending_commands(), vec![UiCommand::Leave]);
        assert!(state.take_credential_ops().contains(&CredentialOp::Forget {
            server: "http://localhost:8080".to_owned()
        }));

        // Correct password is only persisted once RoomsUpdated confirms it.
        state.form.invite_code = "certa".to_owned();
        state.handle_action(UserAction::SubmitOnboarding);
        assert!(state.take_credential_ops().is_empty());
        state.apply_event(UiEvent::RoomsUpdated(vec![]));
        assert_eq!(
            state.take_credential_ops(),
            vec![CredentialOp::Store {
                server: "http://localhost:8080".to_owned(),
                password: "certa".to_owned(),
            }]
        );
    }

    #[test]
    fn paste_invite_link_fills_server_and_password_preserving_name() {
        let mut state = AppState::new(&Settings::default(), None);
        let link = crate::invite::format("https://voz.amigos.example", "café & 123");

        state.handle_action(UserAction::ToggleInviteLink);
        assert!(state.onboarding.link_open);
        state.handle_action(UserAction::PasteInviteLink(link.clone()));
        assert_eq!(state.form.server_url, "https://voz.amigos.example");
        assert_eq!(state.form.invite_code, "café & 123");
        assert_eq!(state.form.display_name, "");
        assert!(!state.show_password_field());
        assert!(!state.onboarding.link_open);

        // Editing server URL afterwards reveals password field again.
        state.form.server_url = "https://outro.example".to_owned();
        assert!(state.show_password_field());

        // Existing display name is preserved when pasting another invite link.
        state.form.display_name = "Maria".to_owned();
        state.handle_action(UserAction::PasteInviteLink(link));
        assert_eq!(state.form.display_name, "Maria");

        // A link without password leaves password field visible.
        state.handle_action(UserAction::PasteInviteLink(
            "zenha://entrar?servidor=https%3A%2F%2Fsem-senha.example".to_owned(),
        ));
        assert_eq!(state.form.server_url, "https://sem-senha.example");
        assert!(state.show_password_field());
    }

    #[test]
    fn copy_invite_link_uses_current_password_and_change_server_keeps_fields() {
        let mut state = AppState::new(&valid_settings(), Some("segredo-salvo".to_owned()));
        state.handle_action(UserAction::OpenSettings);
        state.handle_action(UserAction::CopyInviteLink);
        assert!(state.invite_copied);
        assert_eq!(
            crate::invite::parse(&state.invite_link()),
            Some(crate::invite::Invite {
                server: "http://localhost:8080".to_owned(),
                password: "segredo-salvo".to_owned(),
            })
        );

        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::ChangeServer),
            vec![UiCommand::PlayCue(Cue::SelfLeft), UiCommand::Leave]
        );
        assert_eq!(state.screen, Screen::Onboarding);
        assert!(!state.invite_copied);
        assert_eq!(state.form.display_name, "Pedro");
        assert_eq!(state.form.server_url, "http://localhost:8080");
        assert!(!state.show_password_field());
    }

    #[test]
    fn build_default_server_prefills_empty_onboarding_without_overwriting_saved() {
        let state = AppState::new_with_build_default(
            &Settings::default(),
            None,
            Some("  https://voz.padrao.com  "),
        );
        assert_eq!(state.screen, Screen::Onboarding);
        assert_eq!(state.form.server_url, "https://voz.padrao.com");

        let saved_state = AppState::new_with_build_default(
            &valid_settings(),
            None,
            Some("https://voz.padrao.com"),
        );
        assert_eq!(saved_state.form.server_url, "http://localhost:8080");
    }

    #[test]
    fn startup_invite_link_without_name_prefills_onboarding_and_hides_password() {
        let mut state = AppState::new_with_build_default(&Settings::default(), None, None);
        state.apply_startup_link(StartupLink::Invite(crate::invite::Invite {
            server: "https://voz.exemplo.com".to_owned(),
            password: "segredo".to_owned(),
        }));
        assert_eq!(state.screen, Screen::Onboarding);
        assert_eq!(state.form.server_url, "https://voz.exemplo.com");
        assert!(!state.show_password_field());
        assert_eq!(state.credentials().invite_code, "segredo");
        assert!(state.take_pending_commands().is_empty());
    }

    #[test]
    fn startup_invite_link_with_valid_name_switches_server_and_enters_lobby() {
        let mut state =
            AppState::new_with_build_default(&valid_settings(), Some("antiga".to_owned()), None);
        assert_eq!(state.screen, Screen::Lobby);

        state.apply_startup_link(StartupLink::Invite(crate::invite::Invite {
            server: "https://novo.servidor.com".to_owned(),
            password: "nova".to_owned(),
        }));

        assert_eq!(state.screen, Screen::Lobby);
        let creds = state.credentials();
        assert_eq!(creds.token_server_url, "https://novo.servidor.com");
        assert_eq!(creds.display_name, "Pedro");
        assert_eq!(creds.invite_code, "nova");
        assert_eq!(state.take_pending_commands(), vec![UiCommand::RefreshRooms]);
    }

    #[test]
    fn startup_invalid_link_shows_error_without_disrupting_startup() {
        let mut onboarding = AppState::new_with_build_default(&Settings::default(), None, None);
        onboarding.apply_startup_link(StartupLink::Invalid("zenha://quebrado".to_owned()));
        assert_eq!(onboarding.screen, Screen::Onboarding);
        assert!(onboarding.onboarding.link_open);
        assert_eq!(onboarding.onboarding.link_input, "zenha://quebrado");
        assert_eq!(
            onboarding.onboarding.link_error.as_deref(),
            Some("Link de convite inválido.")
        );

        let mut lobby =
            AppState::new_with_build_default(&valid_settings(), Some("segredo".to_owned()), None);
        lobby.apply_startup_link(StartupLink::Invalid("algo-errado".to_owned()));
        assert_eq!(lobby.screen, Screen::Lobby);
        assert_eq!(
            lobby.lobby_error.as_deref(),
            Some("Link de convite inválido.")
        );
    }

    #[test]
    fn state_types_debug_redacts_secrets() {
        let form = LoginForm {
            display_name: "Ana".to_owned(),
            server_url: "https://voz.exemplo".to_owned(),
            invite_code: "form-secret-code".to_owned(),
        };
        let form_dbg = format!("{form:?}");
        assert!(!form_dbg.contains("form-secret-code"));
        assert!(form_dbg.contains("Ana"));
        assert!(form_dbg.contains("<redacted>"));

        let onboarding = OnboardingState {
            link_open: true,
            link_input: "zenha://entrar?servidor=https://voz.exemplo&senha=link-secret-password"
                .to_owned(),
            ..OnboardingState::default()
        };
        let onboarding_dbg = format!("{onboarding:?}");
        assert!(!onboarding_dbg.contains("link-secret-password"));
        assert!(onboarding_dbg.contains("<redacted>"));

        let op = CredentialOp::Store {
            server: "https://voz.exemplo".to_owned(),
            password: "store-secret-password".to_owned(),
        };
        let op_dbg = format!("{op:?}");
        assert!(!op_dbg.contains("store-secret-password"));
        assert!(op_dbg.contains("https://voz.exemplo"));
        assert!(op_dbg.contains("<redacted>"));
    }

    #[test]
    fn save_name_button_needs_a_changed_valid_name() {
        let mut state = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        state.handle_action(UserAction::OpenSettings);
        assert_eq!(state.name_draft, "Pedro");
        assert!(!state.can_save_name());
        state.name_draft = "  Pedro ".to_owned();
        assert!(!state.can_save_name());
        state.name_draft = "   ".to_owned();
        assert!(!state.can_save_name());
        assert!(display_name_error(&state.name_draft).is_some());
        state.name_draft = "a".repeat(MAX_DISPLAY_NAME_LEN + 1);
        assert!(!state.can_save_name());
        assert!(state.handle_action(UserAction::SaveDisplayName).is_empty());
        assert!(state.take_pending_save().is_none());
        state.name_draft = " Pedrão ".to_owned();
        assert!(state.can_save_name());
    }

    #[test]
    fn saving_name_in_room_persists_renames_self_and_tells_the_core() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("ana")],
        });
        state.take_pending_save();
        state.name_draft = " Pedrão ".to_owned();
        assert_eq!(
            state.handle_action(UserAction::SaveDisplayName),
            vec![UiCommand::SetDisplayName {
                name: "Pedrão".to_owned()
            }]
        );
        assert_eq!(state.form.display_name, "Pedrão");
        assert_eq!(state.name_draft, "Pedrão");
        assert_eq!(state.take_pending_save().unwrap().display_name, "Pedrão");
        assert!(!state.can_save_name());
        let me = room(&state)
            .participants
            .iter()
            .find(|p| p.identity == "pedro")
            .unwrap();
        assert_eq!(me.name, "Pedrão");
        assert_eq!(state.credentials().display_name, "Pedrão");

        let mut renamed = person("ana");
        renamed.name = "Ana Paula".to_owned();
        state.apply_event(UiEvent::ParticipantUpdated(renamed));
        assert!(
            room(&state)
                .participants
                .iter()
                .any(|p| p.identity == "ana" && p.name == "Ana Paula")
        );
    }
}
