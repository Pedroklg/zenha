//! Salas com senha: diálogo de senha, chaves salvas e ordem das salas-filhas na lista.

use std::fmt;

use rooms_protocol::{RoomSummary, ValidationError, validate_room_password};

use super::{AppState, Screen};
use crate::ui_model::UiCommand;

/// Texto digitado que não pode aparecer em `Debug` (senhas).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretText(pub String);

impl fmt::Debug for SecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomPasswordDialog {
    pub room: String,
    pub title: String,
    pub password: SecretText,
    pub error: Option<String>,
    pub busy: bool,
}

/// Salas na ordem da lista lateral: cada sala de primeiro nível seguida das filhas (nível 1).
pub fn sidebar_order(rooms: &[RoomSummary]) -> Vec<(usize, &RoomSummary, u8)> {
    let is_top = |r: &RoomSummary| {
        r.parent
            .as_deref()
            .is_none_or(|p| !rooms.iter().any(|o| o.name == p && o.parent.is_none()))
    };
    let mut ordered = Vec::with_capacity(rooms.len());
    for (idx, room) in rooms.iter().enumerate().filter(|(_, r)| is_top(r)) {
        ordered.push((idx, room, 0));
        ordered.extend(
            rooms
                .iter()
                .enumerate()
                .filter(|(_, c)| !is_top(c) && c.parent.as_deref() == Some(room.name.as_str()))
                .map(|(i, c)| (i, c, 1)),
        );
    }
    ordered
}

pub(super) fn room_password_error(password: &str) -> Option<String> {
    match validate_room_password(password) {
        Ok(()) => None,
        Err(ValidationError::Empty) => Some("Digite a senha da sala.".to_owned()),
        Err(ValidationError::TooLong { max }) => Some(format!("Use no máximo {max} caracteres.")),
        Err(ValidationError::InvalidCharacter) => {
            Some("A senha contém caracteres inválidos.".to_owned())
        }
    }
}

impl AppState {
    pub(super) fn needs_room_password(&self, room: &str) -> bool {
        self.rooms
            .iter()
            .any(|r| r.name == room && r.needs_password())
    }

    pub(super) fn open_room_password(&mut self, room: String, error: Option<String>) {
        let title = super::room_display_title(&self.rooms, &room).to_owned();
        self.room_password = Some(RoomPasswordDialog {
            room,
            title,
            password: SecretText::default(),
            error,
            busy: false,
        });
    }

    pub(super) fn submit_room_password(&mut self) -> Vec<UiCommand> {
        let Some(dialog) = &mut self.room_password else {
            return Vec::new();
        };
        if dialog.busy {
            return Vec::new();
        }
        if let Some(error) = room_password_error(&dialog.password.0) {
            dialog.error = Some(error);
            return Vec::new();
        }
        dialog.busy = true;
        dialog.error = None;
        vec![UiCommand::UnlockRoom {
            room: dialog.room.clone(),
            password: std::mem::take(&mut dialog.password.0),
        }]
    }

    pub(super) fn on_room_unlocked(&mut self, owner: &str, key: &str) -> Vec<UiCommand> {
        let server = self.form.server_url.trim().to_owned();
        self.room_keys.insert(&server, owner, key);
        self.pending_save = Some(self.settings_snapshot());
        let Some(dialog) = self.room_password.take() else {
            return Vec::new();
        };
        match &self.screen {
            Screen::Lobby => self.join_open_room(dialog.room),
            Screen::InRoom(_) => self.switch_open_room(dialog.room),
            Screen::Onboarding | Screen::Connecting { .. } => Vec::new(),
        }
    }

    pub(super) fn on_room_unlock_failed(&mut self, message: String) {
        if let Some(dialog) = &mut self.room_password {
            dialog.busy = false;
            dialog.error = Some(message);
        }
    }

    pub(super) fn on_room_locked(&mut self, room: String) {
        let server = self.form.server_url.trim().to_owned();
        let parent = self
            .rooms
            .iter()
            .find(|r| r.name == room)
            .and_then(|r| r.parent.clone());
        let stale: Vec<&str> = [Some(room.as_str()), parent.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if self.room_keys.remove(&server, &stale) {
            self.pending_save = Some(self.settings_snapshot());
        }
        self.pending_move_notice = None;
        self.leave_to_lobby();
        self.open_room_password(room, None);
    }
}

#[cfg(test)]
mod tests {
    use rooms_core::RoomKey;

    use super::super::test_support::{state_with_code, valid_settings};
    use super::super::{AppState, Screen, UserAction};
    use super::*;
    use crate::ui_model::UiEvent;

    fn summary(name: &str, parent: Option<&str>, locked: bool) -> RoomSummary {
        RoomSummary {
            name: name.to_owned(),
            title: name.to_uppercase(),
            participant_count: 2,
            locked,
            parent: parent.map(str::to_owned),
            ..RoomSummary::default()
        }
    }

    fn lobby_with_locked_rooms() -> AppState {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.apply_event(UiEvent::RoomsUpdated(vec![
            summary("geral", None, false),
            summary("vip", None, true),
            summary("cantinho", Some("vip"), true),
        ]));
        state
    }

    #[test]
    fn sidebar_groups_children_under_their_parent() {
        let rooms = vec![
            summary("filha", Some("mae"), false),
            summary("geral", None, false),
            summary("mae", None, false),
            summary("orfa", Some("sumiu"), false),
            summary("neta", Some("filha"), false),
        ];
        let order: Vec<(&str, u8)> = sidebar_order(&rooms)
            .into_iter()
            .map(|(_, r, depth)| (r.name.as_str(), depth))
            .collect();
        assert_eq!(
            order,
            vec![
                ("geral", 0),
                ("mae", 0),
                ("filha", 1),
                ("orfa", 0),
                ("neta", 0)
            ]
        );
    }

    #[test]
    fn clicking_a_locked_room_without_key_opens_the_password_dialog() {
        let mut state = lobby_with_locked_rooms();
        assert!(
            state
                .handle_action(UserAction::JoinRoom("cantinho".to_owned()))
                .is_empty()
        );
        assert!(matches!(state.screen, Screen::Lobby));
        let dialog = state.room_password.clone().unwrap();
        assert_eq!(dialog.room, "cantinho");
        assert_eq!(dialog.title, "CANTINHO");

        assert!(
            state
                .handle_action(UserAction::SubmitRoomPassword)
                .is_empty()
        );
        assert!(state.room_password.as_ref().unwrap().error.is_some());

        state.room_password.as_mut().unwrap().password.0 = "senha-vip".to_owned();
        assert_eq!(
            state.handle_action(UserAction::SubmitRoomPassword),
            vec![UiCommand::UnlockRoom {
                room: "cantinho".to_owned(),
                password: "senha-vip".to_owned(),
            }]
        );
        assert!(!format!("{:?}", state.room_password).contains("senha-vip"));

        state.apply_event(UiEvent::RoomUnlockFailed {
            message: "Senha da sala incorreta.".to_owned(),
        });
        let dialog = state.room_password.clone().unwrap();
        assert!(!dialog.busy);
        assert_eq!(dialog.error.as_deref(), Some("Senha da sala incorreta."));

        state.handle_action(UserAction::CancelRoomPassword);
        assert!(state.room_password.is_none());
        assert!(
            !state
                .handle_action(UserAction::JoinRoom("geral".to_owned()))
                .is_empty()
        );
    }

    #[test]
    fn unlocking_saves_the_key_and_joins_and_a_rejected_key_is_dropped() {
        let mut state = lobby_with_locked_rooms();
        state.handle_action(UserAction::JoinRoom("cantinho".to_owned()));
        state.room_password.as_mut().unwrap().password.0 = "senha-vip".to_owned();
        state.handle_action(UserAction::SubmitRoomPassword);

        state.apply_event(UiEvent::RoomUnlocked {
            room: "vip".to_owned(),
            key: RoomKey("chave-vip".to_owned()),
        });
        assert!(state.room_password.is_none());
        assert!(matches!(&state.screen, Screen::Connecting { room } if room == "cantinho"));
        assert!(state.take_pending_commands().contains(&UiCommand::Join {
            room: "cantinho".to_owned()
        }));
        let saved = state.take_pending_save().unwrap();
        let server = valid_settings().token_server_url;
        assert_eq!(
            saved
                .room_keys
                .for_server(&server)
                .get("vip")
                .map(String::as_str),
            Some("chave-vip")
        );
        assert!(!format!("{saved:?}").contains("chave-vip"));
        assert_eq!(
            state.credentials().room_keys.get("vip").map(String::as_str),
            Some("chave-vip")
        );

        state.apply_event(UiEvent::RoomLocked {
            room: "cantinho".to_owned(),
        });
        assert!(matches!(state.screen, Screen::Lobby));
        assert_eq!(state.room_password.as_ref().unwrap().room, "cantinho");
        let saved = state.take_pending_save().unwrap();
        assert!(saved.room_keys.for_server(&server).is_empty());
    }
}
