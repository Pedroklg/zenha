//! Administrative authentication, moderation actions and server data packets.

use std::fmt;

use zenha_protocol::{
    ADMIN_CHAT_DELETE_TOPIC, ADMIN_MOVE_TOPIC, ADMIN_STOP_SHARE_TOPIC, AdminChatDelete,
    AdminLoginRequest, AdminMove, AdminStopShare, CreateRoomRequest, CreateWebhookRequest,
    MAX_WEBHOOK_BODY_BYTES, MoveParticipantRequest, ROOMS_CHANGED_TOPIC, RoomsChanged,
    UpdateInviteRequest, UpdateRoomRequest, WEBHOOK_TOPIC, WebhookPost, validate_client_id,
    validate_invite_code, validate_room_name, validate_room_password, validate_room_title,
    validate_webhook_id,
};

use crate::api::{AdminAction, Event, RoomKey, SessionError};
use crate::chat as chat_codec;
use crate::clock::unix_ms;
use crate::net::token::TokenClient;

use super::Session;

pub(super) struct AdminSession {
    token: String,
    expires_at: u64,
}

impl fmt::Debug for AdminSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminSession")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl AdminSession {
    fn is_expired(&self, now_secs: u64) -> bool {
        now_secs >= self.expires_at
    }
}

impl Session {
    pub(super) async fn admin_login(&mut self, password: String) {
        if let Err(err) = validate_client_id(&self.config.client_id) {
            self.emit(Event::CommandFailed(SessionError::InvalidInput(
                err.to_string(),
            )));
            return;
        }
        if password.is_empty() {
            self.emit(Event::CommandFailed(SessionError::InvalidInput(
                "password is empty".to_owned(),
            )));
            return;
        }
        let request = AdminLoginRequest {
            password,
            client_id: self.config.client_id.clone(),
        };
        match self.tokens.admin_login(request).await {
            Ok(response) => {
                self.admin = Some(AdminSession {
                    token: response.token,
                    expires_at: response.expires_at,
                });
                self.emit(Event::AdminChanged { active: true });
            }
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) fn admin_logout(&mut self) {
        if self.admin.take().is_some() {
            self.emit(Event::AdminChanged { active: false });
        }
    }

    fn clear_expired_admin(&mut self) {
        if self.admin.take().is_some() {
            self.emit(Event::AdminChanged { active: false });
        }
        self.emit(Event::CommandFailed(SessionError::AdminUnauthorized));
    }

    pub(super) async fn run_admin_action(&mut self, action: AdminAction) {
        let jwt = match &self.admin {
            Some(admin) if !admin.is_expired(unix_ms() / 1000) => admin.token.clone(),
            Some(_) => {
                self.clear_expired_admin();
                return;
            }
            None => {
                self.emit(Event::CommandFailed(SessionError::AdminUnauthorized));
                return;
            }
        };
        if let Err(err) = validate_admin_action(&action) {
            self.emit(Event::CommandFailed(err));
            return;
        }
        let affects_rooms = matches!(
            action,
            AdminAction::CreateRoom { .. }
                | AdminAction::UpdateRoom { .. }
                | AdminAction::DeleteRoom { .. }
        );
        let new_invite = match &action {
            AdminAction::RotateInvite { new_code } => Some(new_code.clone()),
            _ => None,
        };
        match execute_admin_action(self.tokens.as_ref(), &jwt, action).await {
            Ok(event) => {
                if let Some(code) = new_invite {
                    self.config.invite_code = code;
                }
                if let Some(event) = event {
                    self.emit(event);
                }
                // Lobby users do not receive in-room `rooms.changed` data packets.
                if affects_rooms && !self.connected() {
                    self.refresh_rooms();
                }
            }
            Err(SessionError::AdminUnauthorized) => self.clear_expired_admin(),
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) async fn on_server_data(&mut self, topic: &str, payload: &[u8]) {
        match topic {
            ADMIN_STOP_SHARE_TOPIC
                if payload.is_empty()
                    || serde_json::from_slice::<AdminStopShare>(payload).is_ok() =>
            {
                if self.sharing {
                    self.stop_share().await;
                }
                self.emit(Event::AdminStoppedScreenShare);
            }
            ADMIN_MOVE_TOPIC => {
                if let Ok(mv) = serde_json::from_slice::<AdminMove>(payload)
                    && validate_room_name(&mv.to_room).is_ok()
                {
                    if let Some(unlock) = mv.unlock {
                        self.config
                            .room_keys
                            .insert(unlock.room.clone(), unlock.room_key.clone());
                        self.emit(Event::RoomUnlocked {
                            room: unlock.room,
                            key: RoomKey(unlock.room_key),
                        });
                    }
                    self.emit(Event::MovedByAdmin {
                        to_room: mv.to_room.clone(),
                    });
                    self.join(mv.to_room).await;
                }
            }
            ADMIN_CHAT_DELETE_TOPIC => {
                if let Ok(del) = serde_json::from_slice::<AdminChatDelete>(payload)
                    && chat_codec::validate_chat_id(&del.id).is_ok()
                {
                    self.emit(Event::ChatDeleted {
                        id: del.id,
                        sender: String::new(),
                    });
                }
            }
            ROOMS_CHANGED_TOPIC
                if payload.is_empty()
                    || serde_json::from_slice::<RoomsChanged>(payload).is_ok() =>
            {
                self.schedule_rooms_refresh();
            }
            WEBHOOK_TOPIC => match parse_webhook_post(payload) {
                Some(post) => self.emit(Event::WebhookPosted(post)),
                None => tracing::debug!("dropping invalid webhook payload"),
            },
            _ => {}
        }
    }
}

pub(super) fn is_server_admin_topic(topic: &str) -> bool {
    matches!(
        topic,
        ADMIN_STOP_SHARE_TOPIC
            | ADMIN_MOVE_TOPIC
            | ADMIN_CHAT_DELETE_TOPIC
            | ROOMS_CHANGED_TOPIC
            | WEBHOOK_TOPIC
    )
}

fn parse_webhook_post(payload: &[u8]) -> Option<WebhookPost> {
    if payload.len() > MAX_WEBHOOK_BODY_BYTES * 2 {
        return None;
    }
    let post = serde_json::from_slice::<WebhookPost>(payload).ok()?;
    post.validate().ok()?;
    Some(post)
}

async fn execute_admin_action(
    tokens: &dyn TokenClient,
    jwt: &str,
    action: AdminAction,
) -> Result<Option<Event>, SessionError> {
    match action {
        AdminAction::CreateRoom {
            title,
            password,
            parent,
        } => {
            let request = CreateRoomRequest {
                title,
                password,
                parent,
            };
            let room = tokens.admin_create_room(jwt, request).await?;
            Ok(Some(Event::AdminRoomCreated(room)))
        }
        AdminAction::UpdateRoom {
            id,
            title,
            position,
            password,
            parent,
        } => {
            let request = UpdateRoomRequest {
                title,
                position,
                password,
                parent,
            };
            let room = tokens.admin_update_room(jwt, &id, request).await?;
            Ok(Some(Event::AdminRoomUpdated(room)))
        }
        AdminAction::DeleteRoom { id } => {
            tokens.admin_delete_room(jwt, &id).await?;
            Ok(Some(Event::AdminRoomDeleted { id }))
        }
        AdminAction::MuteParticipant { room, identity } => {
            tokens.admin_mute_participant(jwt, &room, &identity).await?;
            Ok(None)
        }
        AdminAction::StopShareParticipant { room, identity } => {
            tokens
                .admin_stop_share_participant(jwt, &room, &identity)
                .await?;
            Ok(None)
        }
        AdminAction::KickParticipant { room, identity } => {
            tokens.admin_kick_participant(jwt, &room, &identity).await?;
            Ok(None)
        }
        AdminAction::MoveParticipant {
            room,
            identity,
            to_room,
        } => {
            tokens
                .admin_move_participant(jwt, &room, &identity, MoveParticipantRequest { to_room })
                .await?;
            Ok(None)
        }
        AdminAction::DeleteChat { room, id } => {
            tokens.admin_delete_chat(jwt, &room, &id).await?;
            Ok(None)
        }
        AdminAction::RotateInvite { new_code } => {
            tokens
                .admin_rotate_invite(jwt, UpdateInviteRequest { new_code })
                .await?;
            Ok(Some(Event::AdminInviteRotated))
        }
        AdminAction::ListWebhooks => {
            let webhooks = tokens.admin_list_webhooks(jwt).await?;
            Ok(Some(Event::AdminWebhooks(webhooks)))
        }
        AdminAction::CreateWebhook { room, name } => {
            let request = CreateWebhookRequest { room, name };
            let created = tokens.admin_create_webhook(jwt, &request).await?;
            Ok(Some(Event::AdminWebhookCreated(created)))
        }
        AdminAction::DeleteWebhook { id } => {
            tokens.admin_delete_webhook(jwt, &id).await?;
            Ok(Some(Event::AdminWebhookDeleted { id }))
        }
    }
}

fn validate_room_lock(password: Option<&str>, parent: Option<&str>) -> Result<(), SessionError> {
    let invalid = |err: &dyn fmt::Display| SessionError::InvalidInput(err.to_string());
    if let Some(password) = password.filter(|p| !p.is_empty()) {
        validate_room_password(password).map_err(|e| invalid(&e))?;
    }
    if let Some(parent) = parent.filter(|p| !p.is_empty()) {
        validate_room_name(parent).map_err(|e| invalid(&e))?;
    }
    Ok(())
}

fn validate_admin_action(action: &AdminAction) -> Result<(), SessionError> {
    let invalid = |err: &dyn fmt::Display| SessionError::InvalidInput(err.to_string());
    match action {
        AdminAction::CreateRoom {
            title,
            password,
            parent,
        } => {
            validate_room_title(title).map_err(|e| invalid(&e))?;
            validate_room_lock(password.as_deref(), parent.as_deref())
        }
        AdminAction::UpdateRoom {
            id,
            title,
            position,
            password,
            parent,
        } => {
            validate_room_name(id).map_err(|e| invalid(&e))?;
            if title.is_none() && position.is_none() && password.is_none() && parent.is_none() {
                return Err(SessionError::InvalidInput("nothing to update".to_owned()));
            }
            if let Some(title) = title {
                validate_room_title(title).map_err(|e| invalid(&e))?;
            }
            validate_room_lock(password.as_deref(), parent.as_deref())
        }
        AdminAction::DeleteRoom { id } => validate_room_name(id).map_err(|e| invalid(&e)),
        AdminAction::MuteParticipant { room, identity }
        | AdminAction::StopShareParticipant { room, identity }
        | AdminAction::KickParticipant { room, identity } => {
            validate_room_name(room).map_err(|e| invalid(&e))?;
            validate_client_id(identity).map_err(|e| invalid(&e))
        }
        AdminAction::MoveParticipant {
            room,
            identity,
            to_room,
        } => {
            validate_room_name(room).map_err(|e| invalid(&e))?;
            validate_client_id(identity).map_err(|e| invalid(&e))?;
            validate_room_name(to_room).map_err(|e| invalid(&e))?;
            if room == to_room {
                return Err(SessionError::InvalidInput(
                    "destination room matches current room".to_owned(),
                ));
            }
            Ok(())
        }
        AdminAction::DeleteChat { room, id } => {
            validate_room_name(room).map_err(|e| invalid(&e))?;
            chat_codec::validate_chat_id(id)
        }
        AdminAction::RotateInvite { new_code } => {
            validate_invite_code(new_code).map_err(|e| invalid(&e))
        }
        AdminAction::ListWebhooks => Ok(()),
        AdminAction::CreateWebhook { room, name } => CreateWebhookRequest {
            room: room.clone(),
            name: name.clone(),
        }
        .validate()
        .map_err(|e| invalid(&e)),
        AdminAction::DeleteWebhook { id } => validate_webhook_id(id).map_err(|e| invalid(&e)),
    }
}

#[cfg(test)]
mod tests {
    use zenha_protocol::{
        ADMIN_CHAT_DELETE_TOPIC, ADMIN_MOVE_TOPIC, ADMIN_STOP_SHARE_TOPIC, AdminChatDelete,
        AdminMove, AdminWebhook, MAX_WEBHOOK_BODY_BYTES, ROOMS_CHANGED_TOPIC, RoomSummary,
        WEBHOOK_TOPIC, WebhookPost,
    };

    use crate::api::{AdminAction, Command, Event, RoomKey, SessionError};
    use crate::testing::AdminTokenCall;
    use crate::video::{CaptureKind, CaptureTarget, SharePreset};

    use super::super::rooms::ROOMS_CHANGED_DEBOUNCE;
    use super::super::test_support::{data, harness};
    use super::AdminSession;

    #[test]
    fn admin_session_debug_redacts_token() {
        let admin = AdminSession {
            token: "secret-admin-jwt-token".to_owned(),
            expires_at: 1_900_000_000,
        };
        let rendered = format!("{admin:?}");
        assert!(!rendered.contains("secret-admin-jwt-token"));
        assert!(rendered.contains("1900000000"));
    }

    #[tokio::test(start_paused = true)]
    async fn admin_login_logout_and_error_cases() {
        let mut h = harness();
        h.tokens
            .push_admin_login_result(Err(SessionError::AdminInvalidPassword));
        h.send(Command::AdminLogin {
            password: "wrong-password".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::AdminInvalidPassword)
        );

        h.tokens
            .push_admin_login_result(Err(SessionError::AdminDisabled));
        h.send(Command::AdminLogin {
            password: "pw".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::AdminDisabled)
        );

        h.tokens
            .push_admin_login_result(Err(SessionError::AdminRateLimited));
        h.send(Command::AdminLogin {
            password: "pw".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::AdminRateLimited)
        );

        h.send(Command::AdminLogin {
            password: "right-password".to_owned(),
        });
        assert_eq!(h.next().await, Event::AdminChanged { active: true });

        h.send(Command::AdminLogout);
        assert_eq!(h.next().await, Event::AdminChanged { active: false });

        // Action after logout fails immediately without hitting token server.
        let calls_before = h.tokens.admin_calls().len();
        h.send(Command::Admin(AdminAction::DeleteRoom {
            id: "geral".to_owned(),
        }));
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::AdminUnauthorized)
        );
        assert_eq!(h.tokens.admin_calls().len(), calls_before);
    }

    #[tokio::test(start_paused = true)]
    async fn rotated_invite_is_used_for_the_next_token_request() {
        let mut h = harness();
        h.send(Command::AdminLogin {
            password: "admin".to_owned(),
        });
        assert_eq!(h.next().await, Event::AdminChanged { active: true });
        h.send(Command::Admin(AdminAction::RotateInvite {
            new_code: "novo-convite-valido".to_owned(),
        }));
        assert_eq!(h.next().await, Event::AdminInviteRotated);
        h.join("geral").await;
        assert_eq!(
            h.tokens.requests().last().map(|r| r.invite_code.as_str()),
            Some("novo-convite-valido")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn admin_actions_and_expired_or_unauthorized_revert_state() {
        let mut h = harness();
        h.tokens.set_rooms(vec![RoomSummary {
            name: "new-room".to_owned(),
            title: "Sala Nova".to_owned(),
            ..RoomSummary::default()
        }]);
        h.send(Command::AdminLogin {
            password: "admin".to_owned(),
        });
        assert_eq!(h.next().await, Event::AdminChanged { active: true });

        h.send(Command::Admin(AdminAction::CreateRoom {
            title: "Sala Nova".to_owned(),
            password: Some("senha-nova".to_owned()),
            parent: Some("geral".to_owned()),
        }));
        assert_eq!(
            h.next().await,
            Event::AdminRoomCreated(zenha_protocol::AdminRoom {
                id: "new-room".to_owned(),
                title: "Sala Nova".to_owned(),
                position: 0,
                locked: true,
                parent: Some("geral".to_owned()),
            })
        );
        // While in lobby, room mutation refreshes room list preserving `title`.
        let Event::RoomsUpdated(rooms) = h.next().await else {
            panic!("expected RoomsUpdated");
        };
        assert_eq!(rooms[0].title, "Sala Nova");

        h.send(Command::Admin(AdminAction::UpdateRoom {
            id: "new-room".to_owned(),
            title: Some("Renomeada".to_owned()),
            position: Some(1),
            password: Some(String::new()),
            parent: None,
        }));
        assert!(matches!(h.next().await, Event::AdminRoomUpdated(_)));
        assert!(matches!(h.next().await, Event::RoomsUpdated(_)));

        h.send(Command::Admin(AdminAction::RotateInvite {
            new_code: "novo-convite-valido".to_owned(),
        }));
        assert_eq!(h.next().await, Event::AdminInviteRotated);

        h.send(Command::Admin(AdminAction::KickParticipant {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
        }));
        h.send(Command::Admin(AdminAction::MuteParticipant {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
        }));
        h.send(Command::Admin(AdminAction::StopShareParticipant {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
        }));
        h.send(Command::Admin(AdminAction::MoveParticipant {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
            to_room: "jogos".to_owned(),
        }));
        h.send(Command::Admin(AdminAction::DeleteChat {
            room: "geral".to_owned(),
            id: "msg-1".to_owned(),
        }));
        tokio::task::yield_now().await;

        // 403 keeps session active and surfaces AdminForbidden.
        h.tokens
            .push_admin_unit_result(Err(SessionError::AdminForbidden));
        h.send(Command::Admin(AdminAction::DeleteRoom {
            id: "geral".to_owned(),
        }));
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::AdminForbidden)
        );
        tokio::task::yield_now().await;

        // 401 from server revokes in-memory admin session.
        h.tokens
            .push_admin_unit_result(Err(SessionError::AdminUnauthorized));
        h.send(Command::Admin(AdminAction::DeleteRoom {
            id: "geral".to_owned(),
        }));
        assert_eq!(h.next().await, Event::AdminChanged { active: false });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::AdminUnauthorized)
        );

        // Locally expired token fails before sending HTTP request and reverts admin state.
        h.tokens
            .push_admin_login_result(Ok(zenha_protocol::AdminLoginResponse {
                token: "expired-jwt".to_owned(),
                expires_at: 1,
            }));
        h.send(Command::AdminLogin {
            password: "admin".to_owned(),
        });
        assert_eq!(h.next().await, Event::AdminChanged { active: true });
        let calls_before = h.tokens.admin_calls().len();
        h.send(Command::Admin(AdminAction::DeleteRoom {
            id: "geral".to_owned(),
        }));
        assert_eq!(h.next().await, Event::AdminChanged { active: false });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::AdminUnauthorized)
        );
        assert_eq!(h.tokens.admin_calls().len(), calls_before);
        assert!(matches!(
            h.tokens.admin_calls()[1],
            AdminTokenCall::CreateRoom { .. }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn server_admin_data_topics_reject_participants_and_apply_server_packets() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.set_capture_targets(vec![CaptureTarget {
            id: "screen-1".to_owned(),
            name: "Main".to_owned(),
            kind: CaptureKind::Screen,
        }]);
        h.send(Command::StartScreenShare {
            target_id: "screen-1".to_owned(),
            preset: SharePreset::P720F30,
        });
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
        assert!(h.backend.sharing().is_some());

        // Spoofed packets from participants on all 4 server topics must be dropped.
        let spoof_move = serde_json::to_vec(&AdminMove {
            to_room: "jogos".to_owned(),
            unlock: None,
        })
        .unwrap();
        let spoof_del = serde_json::to_vec(&AdminChatDelete {
            id: "msg-spoof".to_owned(),
        })
        .unwrap();
        assert!(h.backend.emit(data(ADMIN_STOP_SHARE_TOPIC, b"{}".to_vec())));
        assert!(h.backend.emit(data(ADMIN_MOVE_TOPIC, spoof_move)));
        assert!(h.backend.emit(data(ADMIN_CHAT_DELETE_TOPIC, spoof_del)));
        assert!(h.backend.emit(data(ROOMS_CHANGED_TOPIC, b"{}".to_vec())));

        // Server-originated admin.chat_delete emits ChatDeleted with empty sender.
        let del_payload = serde_json::to_vec(&AdminChatDelete {
            id: "msg-42".to_owned(),
        })
        .unwrap();
        assert!(
            h.backend
                .emit_server_data(ADMIN_CHAT_DELETE_TOPIC, del_payload)
        );
        assert_eq!(
            h.next().await,
            Event::ChatDeleted {
                id: "msg-42".to_owned(),
                sender: String::new(),
            }
        );
        assert!(h.backend.sharing().is_some());
        tokio::time::advance(ROOMS_CHANGED_DEBOUNCE * 2).await;
        assert_eq!(h.tokens.rooms_calls(), 0);

        // Server-originated admin.stop_share stops share and notifies UI.
        assert!(
            h.backend
                .emit_server_data(ADMIN_STOP_SHARE_TOPIC, b"{}".to_vec())
        );
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.next().await, Event::AdminStoppedScreenShare);
        assert!(h.backend.sharing().is_none());

        // Server-originated admin.move notifies UI and switches room via join.
        h.tokens.lock_room("jogos", "jogos", "senha", "chave-jogos");
        let move_payload = serde_json::to_vec(&AdminMove {
            to_room: "jogos".to_owned(),
            unlock: Some(zenha_protocol::UnlockRoomResponse {
                room: "jogos".to_owned(),
                room_key: "chave-jogos".to_owned(),
            }),
        })
        .unwrap();
        assert!(h.backend.emit_server_data(ADMIN_MOVE_TOPIC, move_payload));
        assert_eq!(
            h.next().await,
            Event::RoomUnlocked {
                room: "jogos".to_owned(),
                key: RoomKey("chave-jogos".to_owned()),
            }
        );
        assert_eq!(
            h.next().await,
            Event::MovedByAdmin {
                to_room: "jogos".to_owned(),
            }
        );
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(
            h.next().await,
            Event::Joined { room, .. } if room == "jogos"
        ));
        assert!(matches!(
            h.next().await,
            Event::ChatHistory { before: None, .. }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn webhook_admin_actions_call_the_server_and_emit_events() {
        let mut h = harness();
        h.send(Command::AdminLogin {
            password: "admin".to_owned(),
        });
        assert_eq!(h.next().await, Event::AdminChanged { active: true });

        let listed = AdminWebhook {
            id: "abc123".to_owned(),
            room: "geral".to_owned(),
            name: "Lobby".to_owned(),
            created_at_unix_ms: 7,
        };
        h.tokens
            .push_admin_webhook_list_result(Ok(vec![listed.clone()]));
        h.send(Command::Admin(AdminAction::ListWebhooks));
        assert_eq!(h.next().await, Event::AdminWebhooks(vec![listed]));

        h.send(Command::Admin(AdminAction::CreateWebhook {
            room: "geral".to_owned(),
            name: "Lobby".to_owned(),
        }));
        let Event::AdminWebhookCreated(created) = h.next().await else {
            panic!("expected AdminWebhookCreated");
        };
        assert_eq!(created.webhook.room, "geral");
        assert_eq!(created.webhook.name, "Lobby");

        h.send(Command::Admin(AdminAction::DeleteWebhook {
            id: "abc123".to_owned(),
        }));
        assert_eq!(
            h.next().await,
            Event::AdminWebhookDeleted {
                id: "abc123".to_owned()
            }
        );

        assert!(matches!(
            h.tokens.admin_calls()[1..],
            [
                AdminTokenCall::ListWebhooks { .. },
                AdminTokenCall::CreateWebhook { .. },
                AdminTokenCall::DeleteWebhook { .. },
            ]
        ));

        let calls_before = h.tokens.admin_calls().len();
        h.send(Command::Admin(AdminAction::CreateWebhook {
            room: "geral".to_owned(),
            name: " ".to_owned(),
        }));
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        h.send(Command::Admin(AdminAction::DeleteWebhook {
            id: "../x".to_owned(),
        }));
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        assert_eq!(h.tokens.admin_calls().len(), calls_before);
    }

    fn webhook_post(id: &str) -> WebhookPost {
        WebhookPost {
            id: id.to_owned(),
            name: "Lobby".to_owned(),
            sent_at_unix_ms: 1,
            content: Some("hello".to_owned()),
            embeds: Vec::new(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn webhook_topic_is_accepted_only_from_the_server_and_validated() {
        let mut h = harness();
        h.join("geral").await;
        let spoof = serde_json::to_vec(&webhook_post("spoof1")).unwrap();
        let invalid = serde_json::to_vec(&WebhookPost {
            content: None,
            ..webhook_post("empty1")
        })
        .unwrap();
        let valid = serde_json::to_vec(&webhook_post("abc123")).unwrap();

        assert!(h.backend.emit(data(WEBHOOK_TOPIC, spoof)));
        assert!(
            h.backend
                .emit_server_data(WEBHOOK_TOPIC, b"not json".to_vec())
        );
        assert!(h.backend.emit_server_data(WEBHOOK_TOPIC, invalid));
        assert!(
            h.backend
                .emit_server_data(WEBHOOK_TOPIC, vec![b' '; MAX_WEBHOOK_BODY_BYTES * 2 + 1])
        );
        assert!(h.backend.emit_server_data(WEBHOOK_TOPIC, valid));
        assert_eq!(h.next().await, Event::WebhookPosted(webhook_post("abc123")));
    }
}
