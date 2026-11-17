//! Admin webhooks: listing, creation (URL shown once) and revocation.

use zenha_core::AdminAction;
use zenha_protocol::{
    AdminWebhook, CreatedWebhook, MAX_WEBHOOKS_COUNT, RoomSummary, ValidationError,
    validate_webhook_name,
};

use super::room_lock::SecretText;
use super::{AdminDialog, AppState};
use crate::ui_model::UiCommand;

pub const DEFAULT_WEBHOOK_NAME: &str = "Gamers Club";

/// URL of a webhook just created; shown once and dropped when the box or the settings close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWebhookNotice {
    pub id: String,
    pub name: String,
    pub room: String,
    pub url: SecretText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhooksState {
    pub items: Vec<AdminWebhook>,
    pub requested: bool,
    pub creating: bool,
    pub new_room: Option<String>,
    pub new_name: String,
    pub created: Option<CreatedWebhookNotice>,
}

impl Default for WebhooksState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            requested: false,
            creating: false,
            new_room: None,
            new_name: DEFAULT_WEBHOOK_NAME.to_owned(),
            created: None,
        }
    }
}

impl WebhooksState {
    /// Room chosen for the next webhook: the picked one while it still exists, else the first room.
    pub fn selected_room<'a>(&'a self, rooms: &'a [RoomSummary]) -> Option<&'a str> {
        self.new_room
            .as_deref()
            .filter(|id| rooms.iter().any(|r| r.name == *id))
            .or_else(|| rooms.first().map(|r| r.name.as_str()))
    }

    pub(super) fn close_session(&mut self) {
        self.requested = false;
        self.created = None;
    }
}

impl AppState {
    pub(super) fn request_webhooks(&mut self) -> Option<UiCommand> {
        if !self.admin.active || self.admin.webhooks.requested {
            return None;
        }
        self.admin.webhooks.requested = true;
        Some(UiCommand::Admin(AdminAction::ListWebhooks))
    }

    pub(super) fn create_webhook(&mut self) -> Vec<UiCommand> {
        if !self.admin.active || self.admin.webhooks.creating {
            return Vec::new();
        }
        let name = self.admin.webhooks.new_name.trim().to_owned();
        let room = self
            .admin
            .webhooks
            .selected_room(&self.rooms)
            .map(str::to_owned);
        let checked = match (room, validate_webhook_name(&name)) {
            (_, Err(ValidationError::Empty)) => Err("Digite o nome do webhook.".to_owned()),
            (_, Err(ValidationError::TooLong { max })) => {
                Err(format!("Use no máximo {max} caracteres."))
            }
            (_, Err(ValidationError::InvalidCharacter)) => {
                Err("O nome contém caracteres inválidos.".to_owned())
            }
            (None, Ok(())) => Err("Escolha a sala do webhook.".to_owned()),
            (Some(_), Ok(())) if self.admin.webhooks.items.len() >= MAX_WEBHOOKS_COUNT => {
                Err(format!(
                    "Limite de {MAX_WEBHOOKS_COUNT} webhooks atingido. Revogue algum antes de criar outro."
                ))
            }
            (Some(room), Ok(())) => Ok(room),
        };
        match checked {
            Ok(room) => {
                self.admin.webhooks.creating = true;
                self.admin.error = None;
                self.admin.notice = None;
                self.admin.webhooks.created = None;
                vec![UiCommand::Admin(AdminAction::CreateWebhook { room, name })]
            }
            Err(message) => {
                self.admin.set_error(message);
                Vec::new()
            }
        }
    }

    pub(super) fn open_revoke_webhook(&mut self, id: String) {
        if !self.admin.active {
            return;
        }
        let Some(webhook) = self.admin.webhooks.items.iter().find(|w| w.id == id) else {
            return;
        };
        self.admin.dialog = Some(AdminDialog::RevokeWebhook {
            name: webhook.name.clone(),
            id,
        });
    }

    pub(super) fn on_webhooks_listed(&mut self, webhooks: Vec<AdminWebhook>) {
        if self.admin.active {
            self.admin.webhooks.items = webhooks;
        }
    }

    pub(super) fn on_webhook_created(&mut self, created: CreatedWebhook) {
        self.admin.webhooks.creating = false;
        if !self.admin.active {
            return;
        }
        let CreatedWebhook { webhook, url } = created;
        self.admin.error = None;
        self.admin.webhooks.items.retain(|w| w.id != webhook.id);
        self.admin.webhooks.created = Some(CreatedWebhookNotice {
            id: webhook.id.clone(),
            name: webhook.name.clone(),
            room: webhook.room.clone(),
            url: SecretText(url),
        });
        self.admin.webhooks.items.push(webhook);
    }

    pub(super) fn on_webhook_deleted(&mut self, id: &str) {
        self.admin.webhooks.items.retain(|w| w.id != id);
        if self
            .admin
            .webhooks
            .created
            .as_ref()
            .is_some_and(|c| c.id == id)
        {
            self.admin.webhooks.created = None;
        }
        if matches!(
            &self.admin.dialog,
            Some(AdminDialog::RevokeWebhook { id: open, .. }) if open == id
        ) {
            self.admin.dialog = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::state_with_code;
    use crate::ui_model::UiEvent;

    fn webhook(id: &str, room: &str) -> AdminWebhook {
        AdminWebhook {
            id: id.to_owned(),
            room: room.to_owned(),
            name: format!("Hook {id}"),
            created_at_unix_ms: 1,
        }
    }

    fn created(id: &str, room: &str) -> CreatedWebhook {
        CreatedWebhook {
            webhook: webhook(id, room),
            url: format!("https://voz.exemplo/hooks/{id}/token-secreto-{id}"),
        }
    }

    fn admin_state() -> AppState {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.apply_event(UiEvent::RoomsUpdated(vec![
            RoomSummary {
                name: "geral".to_owned(),
                title: "Geral".to_owned(),
                ..RoomSummary::default()
            },
            RoomSummary {
                name: "jogos".to_owned(),
                title: "Jogos".to_owned(),
                ..RoomSummary::default()
            },
        ]));
        state.apply_event(UiEvent::AdminChanged { active: true });
        state
    }

    fn list_command() -> UiCommand {
        UiCommand::Admin(AdminAction::ListWebhooks)
    }

    #[test]
    fn opening_settings_as_admin_lists_webhooks_once_per_opening() {
        let mut state = admin_state();
        assert_eq!(
            state.handle_action(UserAction::OpenSettings),
            vec![UiCommand::RefreshAudioDevices, list_command()]
        );
        state.apply_event(UiEvent::AdminWebhooks(vec![webhook("a1", "geral")]));
        assert_eq!(state.admin.webhooks.items, vec![webhook("a1", "geral")]);
        assert!(state.request_webhooks().is_none());

        state.handle_action(UserAction::CloseSettings);
        assert_eq!(
            state.handle_action(UserAction::OpenSettings),
            vec![UiCommand::RefreshAudioDevices, list_command()]
        );
    }

    #[test]
    fn non_admin_settings_do_not_list_webhooks() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        assert_eq!(
            state.handle_action(UserAction::OpenSettings),
            vec![UiCommand::RefreshAudioDevices]
        );
        state.apply_event(UiEvent::AdminWebhooks(vec![webhook("a1", "geral")]));
        assert!(state.admin.webhooks.items.is_empty());
    }

    #[test]
    fn admin_login_with_settings_open_lists_webhooks() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::OpenSettings);
        state.apply_event(UiEvent::AdminChanged { active: true });
        assert!(state.take_pending_commands().contains(&list_command()));
    }

    #[test]
    fn create_sends_room_and_trimmed_name_then_shows_the_url_once() {
        let mut state = admin_state();
        state.admin.webhooks.new_name = "  Torneio ".to_owned();
        state.admin.webhooks.new_room = Some("jogos".to_owned());
        assert_eq!(
            state.handle_action(UserAction::CreateWebhook),
            vec![UiCommand::Admin(AdminAction::CreateWebhook {
                room: "jogos".to_owned(),
                name: "Torneio".to_owned(),
            })]
        );
        assert!(state.admin.webhooks.creating);
        assert!(state.handle_action(UserAction::CreateWebhook).is_empty());

        state.apply_event(UiEvent::AdminWebhookCreated(created("n1", "jogos")));
        let webhooks = &state.admin.webhooks;
        assert!(!webhooks.creating);
        assert_eq!(webhooks.items, vec![webhook("n1", "jogos")]);
        let notice = webhooks.created.as_ref();
        assert_eq!(
            notice.map(|n| n.url.0.as_str()),
            Some("https://voz.exemplo/hooks/n1/token-secreto-n1")
        );
    }

    #[test]
    fn create_defaults_to_the_first_room_and_default_name() {
        let mut state = admin_state();
        assert_eq!(state.admin.webhooks.new_name, DEFAULT_WEBHOOK_NAME);
        assert_eq!(
            state.handle_action(UserAction::CreateWebhook),
            vec![UiCommand::Admin(AdminAction::CreateWebhook {
                room: "geral".to_owned(),
                name: DEFAULT_WEBHOOK_NAME.to_owned(),
            })]
        );
    }

    #[test]
    fn create_rejects_bad_input_and_requires_admin() {
        let mut state = admin_state();
        state.admin.webhooks.new_name = "   ".to_owned();
        assert!(state.handle_action(UserAction::CreateWebhook).is_empty());
        assert_eq!(
            state.admin.error.as_deref(),
            Some("Digite o nome do webhook.")
        );

        state.admin.webhooks.new_name = "x".repeat(33);
        assert!(state.handle_action(UserAction::CreateWebhook).is_empty());
        assert_eq!(
            state.admin.error.as_deref(),
            Some("Use no máximo 32 caracteres.")
        );

        state.admin.webhooks.new_name = "ok".to_owned();
        state.admin.webhooks.items = (0..MAX_WEBHOOKS_COUNT)
            .map(|i| webhook(&format!("w{i}"), "geral"))
            .collect();
        assert!(state.handle_action(UserAction::CreateWebhook).is_empty());
        assert!(
            state
                .admin
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Limite"))
        );

        let mut other = state_with_code();
        other.handle_action(UserAction::SubmitOnboarding);
        assert!(other.handle_action(UserAction::CreateWebhook).is_empty());
        assert_eq!(other.admin.error, None);
    }

    #[test]
    fn revoke_asks_for_confirmation_then_removes_on_deleted_event() {
        let mut state = admin_state();
        state.apply_event(UiEvent::AdminWebhooks(vec![
            webhook("a1", "geral"),
            webhook("a2", "jogos"),
        ]));
        assert!(
            state
                .handle_action(UserAction::OpenRevokeWebhook("nope".to_owned()))
                .is_empty()
        );
        assert_eq!(state.admin.dialog, None);

        state.handle_action(UserAction::OpenRevokeWebhook("a1".to_owned()));
        assert_eq!(
            state.admin.dialog,
            Some(AdminDialog::RevokeWebhook {
                id: "a1".to_owned(),
                name: "Hook a1".to_owned(),
            })
        );
        assert_eq!(
            state.handle_action(UserAction::ConfirmAdminDialog),
            vec![UiCommand::Admin(AdminAction::DeleteWebhook {
                id: "a1".to_owned(),
            })]
        );
        assert_eq!(state.admin.webhooks.items.len(), 2);

        state.apply_event(UiEvent::AdminWebhookDeleted {
            id: "a1".to_owned(),
        });
        assert_eq!(state.admin.webhooks.items, vec![webhook("a2", "jogos")]);
    }

    #[test]
    fn created_url_is_dropped_on_dismiss_close_settings_and_logout() {
        let mut state = admin_state();
        let show = |state: &mut AppState| {
            state.apply_event(UiEvent::AdminWebhookCreated(created("n1", "geral")));
            assert!(state.admin.webhooks.created.is_some());
        };

        show(&mut state);
        state.handle_action(UserAction::DismissCreatedWebhook);
        assert!(state.admin.webhooks.created.is_none());

        state.handle_action(UserAction::OpenSettings);
        show(&mut state);
        state.handle_action(UserAction::CloseSettings);
        assert!(state.admin.webhooks.created.is_none());
        assert_eq!(state.admin.webhooks.items.len(), 1);

        show(&mut state);
        state.apply_event(UiEvent::AdminChanged { active: false });
        assert!(state.admin.webhooks.created.is_none());
        assert!(state.admin.webhooks.items.is_empty());
    }

    #[test]
    fn created_url_is_not_stored_when_admin_is_gone_and_never_reaches_debug() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.apply_event(UiEvent::AdminWebhookCreated(created("n1", "geral")));
        assert!(state.admin.webhooks.created.is_none());

        let mut admin = admin_state();
        admin.apply_event(UiEvent::AdminWebhookCreated(created("n2", "geral")));
        let dump = format!("{:?}", admin.admin);
        assert!(!dump.contains("token-secreto"));
        assert!(
            !format!("{:?}", UiEvent::AdminWebhookCreated(created("n2", "geral")))
                .contains("token-secreto")
        );
    }

    #[test]
    fn deleting_the_webhook_whose_url_is_open_closes_the_url_box() {
        let mut state = admin_state();
        state.apply_event(UiEvent::AdminWebhookCreated(created("n1", "geral")));
        state.apply_event(UiEvent::AdminWebhookDeleted {
            id: "n1".to_owned(),
        });
        assert!(state.admin.webhooks.created.is_none());
        assert!(state.admin.webhooks.items.is_empty());
    }

    #[test]
    fn failure_while_creating_releases_the_create_button() {
        let mut state = admin_state();
        state.handle_action(UserAction::CreateWebhook);
        assert!(state.admin.webhooks.creating);
        state.apply_event(UiEvent::AdminFailed {
            message: "falhou".to_owned(),
        });
        assert!(!state.admin.webhooks.creating);
    }

    #[test]
    fn selected_room_falls_back_when_the_picked_room_disappears() {
        let mut state = admin_state();
        state.admin.webhooks.new_room = Some("jogos".to_owned());
        assert_eq!(
            state.admin.webhooks.selected_room(&state.rooms),
            Some("jogos")
        );
        state.admin.webhooks.new_room = Some("sumiu".to_owned());
        assert_eq!(
            state.admin.webhooks.selected_room(&state.rooms),
            Some("geral")
        );
        assert_eq!(state.admin.webhooks.selected_room(&[]), None);
    }
}
