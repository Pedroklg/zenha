//! Estado administrativo, validação de salas/convites e diálogos de moderação.

use std::fmt;

use rooms_core::AdminAction;
use rooms_protocol::{
    AdminRoom, InviteCodeError, RoomSummary, ValidationError, validate_invite_code,
    validate_room_title,
};

use super::{AppState, Screen};
use crate::ui_model::UiCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminDialog {
    CreateRoom {
        title: String,
        error: Option<String>,
    },
    RenameRoom {
        id: String,
        title: String,
        error: Option<String>,
    },
    DeleteRoom {
        id: String,
        title: String,
    },
    KickParticipant {
        room: String,
        identity: String,
        name: String,
    },
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct AdminState {
    pub active: bool,
    pub logging_in: bool,
    pub password_input: String,
    pub new_invite_input: String,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub dialog: Option<AdminDialog>,
}

impl AdminState {
    pub fn reset(&mut self) {
        self.active = false;
        self.logging_in = false;
        self.password_input.clear();
        self.new_invite_input.clear();
        self.error = None;
        self.notice = None;
        self.dialog = None;
    }

    pub(super) fn set_error(&mut self, message: String) {
        self.error = Some(message);
        self.notice = None;
    }

    pub(super) fn set_notice(&mut self, message: String) {
        self.notice = Some(message);
        self.error = None;
    }
}

impl fmt::Debug for AdminState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminState")
            .field("active", &self.active)
            .field("logging_in", &self.logging_in)
            .field("password_input", &"<redacted>")
            .field("new_invite_input", &"<redacted>")
            .field("error", &self.error)
            .field("notice", &self.notice)
            .field("dialog", &self.dialog)
            .finish()
    }
}

impl AppState {
    pub(super) fn admin_login(&mut self) -> Vec<UiCommand> {
        let password = std::mem::take(&mut self.admin.password_input);
        if password.is_empty() {
            self.admin
                .set_error("Digite a senha de administrador.".to_owned());
            return Vec::new();
        }
        self.admin.logging_in = true;
        self.admin.error = None;
        self.admin.notice = None;
        vec![UiCommand::AdminLogin { password }]
    }

    pub(super) fn admin_logout(&mut self) -> Vec<UiCommand> {
        if !self.admin.active && !self.admin.logging_in {
            return Vec::new();
        }
        self.admin.new_invite_input.clear();
        self.admin.dialog = None;
        self.admin.error = None;
        self.admin.notice = None;
        vec![UiCommand::AdminLogout]
    }

    pub(super) fn rotate_invite(&mut self) -> Vec<UiCommand> {
        if !self.admin.active {
            return Vec::new();
        }
        if let Err(message) = validate_invite_code_pt(&self.admin.new_invite_input) {
            self.admin.set_error(message);
            return Vec::new();
        }
        self.admin.error = None;
        self.admin.notice = None;
        let new_code = self.admin.new_invite_input.clone();
        vec![UiCommand::Admin(AdminAction::RotateInvite { new_code })]
    }

    pub(super) fn move_room(&mut self, id: &str, delta: isize) -> Vec<UiCommand> {
        if !self.admin.active {
            return Vec::new();
        }
        let Some(pos) = self.rooms.iter().position(|r| r.name == id) else {
            return Vec::new();
        };
        let target = pos as isize + delta;
        if target < 0 || target as usize >= self.rooms.len() {
            return Vec::new();
        }
        vec![UiCommand::Admin(AdminAction::UpdateRoom {
            id: id.to_owned(),
            title: None,
            position: Some(target as usize),
        })]
    }

    pub(super) fn confirm_admin_dialog(&mut self) -> Vec<UiCommand> {
        if !self.admin.active {
            self.admin.dialog = None;
            return Vec::new();
        }
        let Some(dialog) = self.admin.dialog.take() else {
            return Vec::new();
        };
        match dialog {
            AdminDialog::CreateRoom { title, .. } => match validate_room_title_pt(&title) {
                Ok(trimmed) => {
                    vec![UiCommand::Admin(AdminAction::CreateRoom { title: trimmed })]
                }
                Err(message) => {
                    self.admin.dialog = Some(AdminDialog::CreateRoom {
                        title,
                        error: Some(message),
                    });
                    Vec::new()
                }
            },
            AdminDialog::RenameRoom { id, title, .. } => match validate_room_title_pt(&title) {
                Ok(trimmed) => vec![UiCommand::Admin(AdminAction::UpdateRoom {
                    id,
                    title: Some(trimmed),
                    position: None,
                })],
                Err(message) => {
                    self.admin.dialog = Some(AdminDialog::RenameRoom {
                        id,
                        title,
                        error: Some(message),
                    });
                    Vec::new()
                }
            },
            AdminDialog::DeleteRoom { id, .. } => {
                vec![UiCommand::Admin(AdminAction::DeleteRoom { id })]
            }
            AdminDialog::KickParticipant { room, identity, .. } => {
                let valid = matches!(
                    &self.screen,
                    Screen::InRoom(state)
                        if state.room == room
                            && !state.local_identity.is_empty()
                            && identity != state.local_identity
                );
                if valid {
                    vec![UiCommand::Admin(AdminAction::KickParticipant {
                        room,
                        identity,
                    })]
                } else {
                    Vec::new()
                }
            }
        }
    }

    pub(super) fn remote_target_room(&self, identity: &str) -> Option<(String, String)> {
        if !self.admin.active {
            return None;
        }
        let Screen::InRoom(room) = &self.screen else {
            return None;
        };
        if room.local_identity.is_empty() || identity == room.local_identity {
            return None;
        }
        let target = room.participants.iter().find(|p| p.identity == identity)?;
        Some((room.room.clone(), target.name.clone()))
    }

    pub(super) fn prune_admin_dialog_for_rooms(&mut self) {
        let missing = match &self.admin.dialog {
            Some(AdminDialog::RenameRoom { id, .. } | AdminDialog::DeleteRoom { id, .. }) => {
                !self.rooms.iter().any(|r| &r.name == id)
            }
            _ => false,
        };
        if missing {
            self.admin.dialog = None;
        }
    }
}

pub(super) fn upsert_admin_room(rooms: &mut Vec<RoomSummary>, room: AdminRoom) {
    let mut entry = rooms
        .iter()
        .position(|r| r.name == room.id)
        .map(|idx| rooms.remove(idx))
        .unwrap_or_else(|| RoomSummary {
            name: room.id,
            title: String::new(),
            participants: Vec::new(),
        });
    entry.title = room.title;
    let pos = room.position.min(rooms.len());
    rooms.insert(pos, entry);
}

fn validate_room_title_pt(title: &str) -> Result<String, String> {
    match validate_room_title(title) {
        Ok(()) => Ok(title.trim().to_owned()),
        Err(ValidationError::Empty) => Err("Digite o nome da sala.".to_owned()),
        Err(ValidationError::TooLong { max }) => Err(format!("Use no máximo {max} caracteres.")),
        Err(ValidationError::InvalidCharacter) => {
            Err("O título contém caracteres inválidos.".to_owned())
        }
    }
}

fn validate_invite_code_pt(code: &str) -> Result<(), String> {
    match validate_invite_code(code) {
        Ok(()) => Ok(()),
        Err(InviteCodeError::Empty) => Err("Digite o novo código de convite.".to_owned()),
        Err(InviteCodeError::TooShort { min }) => {
            Err(format!("O código precisa ter pelo menos {min} caracteres."))
        }
        Err(InviteCodeError::TooLong { max }) => {
            Err(format!("O código deve ter no máximo {max} caracteres."))
        }
        Err(InviteCodeError::InvalidCharacter) => {
            Err("O código contém caracteres inválidos.".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use rooms_core::RemovalReason;

    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{chat_msg, in_room, person, room, state_with_code};
    use crate::ui_model::UiEvent;

    #[test]
    fn admin_login_clears_password_redacts_debug_and_logout_resets_state() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.admin.password_input = "super-secret-admin".to_owned();
        state.admin.new_invite_input = "secret-invite-1234".to_owned();

        let debug_text = format!("{:?}", state.admin);
        assert!(!debug_text.contains("super-secret-admin"));
        assert!(!debug_text.contains("secret-invite-1234"));

        assert_eq!(
            state.handle_action(UserAction::AdminLogin),
            vec![UiCommand::AdminLogin {
                password: "super-secret-admin".to_owned(),
            }]
        );
        assert!(state.admin.password_input.is_empty());
        assert!(state.admin.logging_in);

        state.apply_event(UiEvent::AdminChanged { active: true });
        assert!(state.admin.active);
        assert!(!state.admin.logging_in);
        assert!(state.admin.error.is_none());

        assert_eq!(
            state.handle_action(UserAction::AdminLogout),
            vec![UiCommand::AdminLogout]
        );
        state.apply_event(UiEvent::AdminChanged { active: false });
        assert!(!state.admin.active);
        assert!(!state.admin.logging_in);
        assert!(state.admin.new_invite_input.is_empty());
        assert!(state.admin.dialog.is_none());
    }

    #[test]
    fn admin_expired_emits_error_and_returns_to_non_admin_state() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.apply_event(UiEvent::AdminChanged { active: true });
        assert!(state.admin.active);

        state.apply_event(UiEvent::AdminChanged { active: false });
        state.apply_event(UiEvent::AdminFailed {
            message: "Sessão de administrador expirada. Entre novamente.".to_owned(),
        });

        assert!(!state.admin.active);
        assert_eq!(
            state.admin.error.as_deref(),
            Some("Sessão de administrador expirada. Entre novamente.")
        );
        assert_eq!(
            state.lobby_notice.as_deref(),
            Some("Sessão de administrador expirada. Entre novamente.")
        );
    }

    #[test]
    fn admin_room_actions_require_admin_and_validate_titles() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.apply_event(UiEvent::RoomsUpdated(vec![
            RoomSummary {
                name: "sala-1".to_owned(),
                title: "Café".to_owned(),
                participants: Vec::new(),
            },
            RoomSummary {
                name: "sala-2".to_owned(),
                title: "Jogos".to_owned(),
                participants: Vec::new(),
            },
        ]));

        // Without admin active, all room mutations are ignored.
        assert!(state.handle_action(UserAction::OpenCreateRoom).is_empty());
        assert!(state.admin.dialog.is_none());
        assert!(
            state
                .handle_action(UserAction::MoveRoomDown("sala-1".to_owned()))
                .is_empty()
        );

        state.apply_event(UiEvent::AdminChanged { active: true });

        // Create room validates non-empty title, then sends trimmed title.
        state.handle_action(UserAction::OpenCreateRoom);
        assert!(
            state
                .handle_action(UserAction::ConfirmAdminDialog)
                .is_empty()
        );
        assert!(matches!(
            &state.admin.dialog,
            Some(AdminDialog::CreateRoom { error: Some(_), .. })
        ));
        if let Some(AdminDialog::CreateRoom { title, .. }) = &mut state.admin.dialog {
            *title = "  Música  ".to_owned();
        }
        assert_eq!(
            state.handle_action(UserAction::ConfirmAdminDialog),
            vec![UiCommand::Admin(AdminAction::CreateRoom {
                title: "Música".to_owned(),
            })]
        );
        assert!(state.admin.dialog.is_none());

        // Rename room preserves id and sends trimmed title.
        state.handle_action(UserAction::OpenRenameRoom("sala-1".to_owned()));
        if let Some(AdminDialog::RenameRoom { title, .. }) = &mut state.admin.dialog {
            *title = "Café Novo".to_owned();
        }
        assert_eq!(
            state.handle_action(UserAction::ConfirmAdminDialog),
            vec![UiCommand::Admin(AdminAction::UpdateRoom {
                id: "sala-1".to_owned(),
                title: Some("Café Novo".to_owned()),
                position: None,
            })]
        );

        // Move room down/up respects list bounds.
        assert!(
            state
                .handle_action(UserAction::MoveRoomUp("sala-1".to_owned()))
                .is_empty()
        );
        assert!(
            state
                .handle_action(UserAction::MoveRoomDown("sala-2".to_owned()))
                .is_empty()
        );
        assert_eq!(
            state.handle_action(UserAction::MoveRoomDown("sala-1".to_owned())),
            vec![UiCommand::Admin(AdminAction::UpdateRoom {
                id: "sala-1".to_owned(),
                title: None,
                position: Some(1),
            })]
        );
        assert_eq!(
            state.handle_action(UserAction::MoveRoomUp("sala-2".to_owned())),
            vec![UiCommand::Admin(AdminAction::UpdateRoom {
                id: "sala-2".to_owned(),
                title: None,
                position: Some(0),
            })]
        );

        // Delete room confirms by id while displaying title.
        state.handle_action(UserAction::OpenDeleteRoom("sala-2".to_owned()));
        assert_eq!(
            state.admin.dialog,
            Some(AdminDialog::DeleteRoom {
                id: "sala-2".to_owned(),
                title: "Jogos".to_owned(),
            })
        );
        assert_eq!(
            state.handle_action(UserAction::ConfirmAdminDialog),
            vec![UiCommand::Admin(AdminAction::DeleteRoom {
                id: "sala-2".to_owned(),
            })]
        );
    }

    #[test]
    fn participant_admin_actions_require_admin_and_block_self() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::RoomsUpdated(vec![
            RoomSummary {
                name: "geral".to_owned(),
                title: "Geral".to_owned(),
                participants: Vec::new(),
            },
            RoomSummary {
                name: "sala-2".to_owned(),
                title: "Sala Dois".to_owned(),
                participants: Vec::new(),
            },
        ]));
        state.apply_event(UiEvent::RemoteScreenShareStarted {
            identity: "ana".to_owned(),
        });

        // Denied without admin.
        assert!(
            state
                .handle_action(UserAction::AdminMuteParticipant {
                    identity: "ana".to_owned(),
                })
                .is_empty()
        );

        state.apply_event(UiEvent::AdminChanged { active: true });

        // Self is always blocked even when admin.
        assert!(
            state
                .handle_action(UserAction::AdminMuteParticipant {
                    identity: "pedro".to_owned(),
                })
                .is_empty()
        );
        assert!(
            state
                .handle_action(UserAction::OpenKickParticipant {
                    identity: "pedro".to_owned(),
                })
                .is_empty()
        );
        assert!(state.admin.dialog.is_none());

        // Remote mute works.
        assert_eq!(
            state.handle_action(UserAction::AdminMuteParticipant {
                identity: "ana".to_owned(),
            }),
            vec![UiCommand::Admin(AdminAction::MuteParticipant {
                room: "geral".to_owned(),
                identity: "ana".to_owned(),
            })]
        );

        // Stop share works for ana (who is sharing) and not when she isn't.
        assert_eq!(
            state.handle_action(UserAction::AdminStopShareParticipant {
                identity: "ana".to_owned(),
            }),
            vec![UiCommand::Admin(AdminAction::StopShareParticipant {
                room: "geral".to_owned(),
                identity: "ana".to_owned(),
            })]
        );
        state.apply_event(UiEvent::RemoteScreenShareStopped {
            identity: "ana".to_owned(),
        });
        assert!(
            state
                .handle_action(UserAction::AdminStopShareParticipant {
                    identity: "ana".to_owned(),
                })
                .is_empty()
        );

        // Move participant ignores same room and works for another room.
        assert!(
            state
                .handle_action(UserAction::AdminMoveParticipant {
                    identity: "ana".to_owned(),
                    to_room: "geral".to_owned(),
                })
                .is_empty()
        );
        assert_eq!(
            state.handle_action(UserAction::AdminMoveParticipant {
                identity: "ana".to_owned(),
                to_room: "sala-2".to_owned(),
            }),
            vec![UiCommand::Admin(AdminAction::MoveParticipant {
                room: "geral".to_owned(),
                identity: "ana".to_owned(),
                to_room: "sala-2".to_owned(),
            })]
        );

        // Kick requires confirmation dialog.
        state.handle_action(UserAction::OpenKickParticipant {
            identity: "ana".to_owned(),
        });
        assert!(matches!(
            &state.admin.dialog,
            Some(AdminDialog::KickParticipant { identity, .. }) if identity == "ana"
        ));
        assert_eq!(
            state.handle_action(UserAction::ConfirmAdminDialog),
            vec![UiCommand::Admin(AdminAction::KickParticipant {
                room: "geral".to_owned(),
                identity: "ana".to_owned(),
            })]
        );
    }

    #[test]
    fn admin_deletes_other_saved_chat_and_empty_sender_chat_deleted_removes_it() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(chat_msg(
            "other-unsaved",
            "ana",
            false,
        )));
        state.apply_event(UiEvent::ChatReceived(chat_msg("other-saved", "ana", true)));
        state.apply_event(UiEvent::ChatReceived(chat_msg("own-saved", "pedro", true)));

        state.apply_event(UiEvent::AdminChanged { active: true });

        // Unsaved cannot be deleted even as admin.
        assert!(
            state
                .handle_action(UserAction::DeleteChat("other-unsaved".to_owned()))
                .is_empty()
        );
        // Own saved keeps using normal DeleteChat.
        assert_eq!(
            state.handle_action(UserAction::DeleteChat("own-saved".to_owned())),
            vec![UiCommand::DeleteChat {
                id: "own-saved".to_owned(),
            }]
        );
        // Other's saved uses AdminAction::DeleteChat.
        assert_eq!(
            state.handle_action(UserAction::DeleteChat("other-saved".to_owned())),
            vec![UiCommand::Admin(AdminAction::DeleteChat {
                room: "geral".to_owned(),
                id: "other-saved".to_owned(),
            })]
        );

        // Empty sender from admin.chat_delete removes the message.
        state.apply_event(UiEvent::ChatDeleted {
            id: "other-saved".to_owned(),
            sender: String::new(),
        });
        assert!(!room(&state).chat.contains_text_id("other-saved"));
    }

    #[test]
    fn rotate_invite_validates_and_clears_on_success() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.apply_event(UiEvent::AdminChanged { active: true });

        state.admin.new_invite_input = "curto".to_owned();
        assert!(state.handle_action(UserAction::RotateInvite).is_empty());
        assert!(state.admin.error.is_some());

        state.admin.new_invite_input = "novo-convite-seguro".to_owned();
        assert_eq!(
            state.handle_action(UserAction::RotateInvite),
            vec![UiCommand::Admin(AdminAction::RotateInvite {
                new_code: "novo-convite-seguro".to_owned(),
            })]
        );

        state.apply_event(UiEvent::AdminInviteRotated);
        assert!(state.admin.new_invite_input.is_empty());
        assert_eq!(state.current_password(), "novo-convite-seguro");
        assert!(state.admin.error.is_none());
        assert_eq!(
            state.admin.notice.as_deref(),
            Some("Código de convite atualizado.")
        );
    }

    #[test]
    fn admin_event_notices_and_removed_from_room_update_ui_state() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.apply_event(UiEvent::RoomsUpdated(vec![
            RoomSummary {
                name: "geral".to_owned(),
                title: "Geral".to_owned(),
                participants: Vec::new(),
            },
            RoomSummary {
                name: "sala-2".to_owned(),
                title: "Reunião".to_owned(),
                participants: Vec::new(),
            },
        ]));
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro")],
        });

        state.apply_event(UiEvent::AdminStoppedScreenShare);
        assert_eq!(
            room(&state).chat.notice.as_deref(),
            Some("Um admin parou sua transmissão")
        );

        // Move shows room title and survives room reset when Joined completes.
        state.apply_event(UiEvent::MovedByAdmin {
            to_room: "sala-2".to_owned(),
        });
        state.apply_event(UiEvent::Connecting);
        state.apply_event(UiEvent::Joined {
            room: "sala-2".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro")],
        });
        assert_eq!(room(&state).room, "sala-2");
        assert_eq!(
            room(&state).chat.notice.as_deref(),
            Some("Um admin moveu você para Reunião")
        );

        // Kicked returns to Lobby and keeps notice across RoomsUpdated.
        state.apply_event(UiEvent::Left);
        state.apply_event(UiEvent::RemovedFromRoom {
            reason: RemovalReason::Kicked,
        });
        assert!(matches!(state.screen, Screen::Lobby));
        assert_eq!(
            state.lobby_notice.as_deref(),
            Some("Você foi removido da sala")
        );
        state.apply_event(UiEvent::RoomsUpdated(Vec::new()));
        assert_eq!(
            state.lobby_notice.as_deref(),
            Some("Você foi removido da sala")
        );

        // RoomDeleted when received directly from InRoom also returns to Lobby.
        state.apply_event(UiEvent::RoomsUpdated(vec![RoomSummary {
            name: "sala-2".to_owned(),
            title: "Reunião".to_owned(),
            participants: Vec::new(),
        }]));
        state.handle_action(UserAction::JoinRoom("sala-2".to_owned()));
        state.apply_event(UiEvent::Joined {
            room: "sala-2".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro")],
        });
        state.apply_event(UiEvent::RemovedFromRoom {
            reason: RemovalReason::RoomDeleted,
        });
        assert!(matches!(state.screen, Screen::Lobby));
        assert_eq!(state.lobby_notice.as_deref(), Some("A sala foi apagada"));
    }
}
