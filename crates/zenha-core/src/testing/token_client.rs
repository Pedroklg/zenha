//! Implementação de `TokenClient` para `FakeTokenClient`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use zenha_protocol::{AdminLoginRequest, AdminLoginResponse, AdminRoom, AvatarUploadResponse, ChatHistoryResponse, ChatMessage, CreateRoomRequest, MoveParticipantRequest, RoomSummary, SaveChatRequest, TokenRequest, TokenResponse, UnlockRoomRequest, UnlockRoomResponse, UpdateInviteRequest, UpdateRoomRequest};

use crate::api::SessionError;
use crate::net::token::TokenClient;

use super::lock;
use super::token::FakeTokenClient;

#[async_trait]
impl TokenClient for FakeTokenClient {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse, SessionError> {
        let mut state = lock(&self.state);
        state.requests.push(request.clone());
        if state.reject_invite {
            return Err(SessionError::InviteRejected);
        }
        if state.remaining_failures > 0 {
            state.remaining_failures -= 1;
            return Err(SessionError::TokenServer("fake failure".to_owned()));
        }
        if let Some(room_lock) = state.room_locks.get(&request.room)
            && request.room_key.as_deref() != Some(room_lock.key.as_str())
        {
            return Err(SessionError::RoomLocked(request.room));
        }
        Ok(TokenResponse {
            token: format!("token-{}-{}", request.room, state.requests.len()),
            livekit_url: "ws://fake.invalid".to_owned(),
        })
    }

    async fn fetch_rooms(
        &self,
        _invite_code: &str,
        room_keys: &BTreeMap<String, String>,
    ) -> Result<Vec<RoomSummary>, SessionError> {
        let mut state = lock(&self.state);
        state.rooms_calls += 1;
        state.rooms_keys.push(room_keys.clone());
        if state.reject_invite {
            return Err(SessionError::InviteRejected);
        }
        Ok(state.rooms.clone())
    }

    async fn unlock_room(
        &self,
        room: &str,
        request: UnlockRoomRequest,
    ) -> Result<UnlockRoomResponse, SessionError> {
        let state = lock(&self.state);
        match state.room_locks.get(room) {
            Some(room_lock) if room_lock.password == request.password => Ok(UnlockRoomResponse {
                room: room_lock.owner.clone(),
                room_key: room_lock.key.clone(),
            }),
            Some(_) => Err(SessionError::RoomPasswordRejected),
            None => Err(SessionError::TokenServer("room has no password".to_owned())),
        }
    }

    async fn save_chat(
        &self,
        jwt: &str,
        room: &str,
        request: SaveChatRequest,
    ) -> Result<ChatMessage, SessionError> {
        let mut state = lock(&self.state);
        state
            .save_calls
            .push((jwt.to_owned(), room.to_owned(), request.clone()));
        if let Some(result) = state.save_results.pop_front() {
            return result;
        }
        let sender = state
            .requests
            .iter()
            .rev()
            .find(|req| req.room == room)
            .map_or_else(|| "local".to_owned(), |req| req.display_name.clone());
        Ok(ChatMessage {
            id: request.id,
            sender,
            sent_at_unix_ms: request.sent_at_unix_ms,
            text: request.text,
            saved: true,
        })
    }

    async fn fetch_chat_history(
        &self,
        jwt: &str,
        room: &str,
        before: Option<&str>,
    ) -> Result<ChatHistoryResponse, SessionError> {
        let mut state = lock(&self.state);
        let cursor = before.map(str::to_owned);
        state
            .history_calls
            .push((jwt.to_owned(), room.to_owned(), cursor.clone()));
        if let Some(result) = state.history_results.pop_front() {
            return result;
        }
        Ok(state
            .chat_history_pages
            .get(&cursor)
            .cloned()
            .unwrap_or(ChatHistoryResponse {
                messages: Vec::new(),
                has_more: false,
            }))
    }

    async fn delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state
            .delete_calls
            .push((jwt.to_owned(), room.to_owned(), id.to_owned()));
        if let Some(result) = state.delete_results.pop_front() {
            return result;
        }
        Ok(())
    }

    async fn admin_login(
        &self,
        request: AdminLoginRequest,
    ) -> Result<AdminLoginResponse, SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::Login(request));
        if let Some(result) = state.admin_login_results.pop_front() {
            return result;
        }
        Ok(AdminLoginResponse {
            token: "admin-jwt-1".to_owned(),
            expires_at: 9_999_999_999,
        })
    }

    async fn admin_create_room(
        &self,
        jwt: &str,
        request: CreateRoomRequest,
    ) -> Result<AdminRoom, SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::CreateRoom {
            jwt: jwt.to_owned(),
            request: request.clone(),
        });
        if let Some(result) = state.admin_room_results.pop_front() {
            return result;
        }
        Ok(AdminRoom {
            id: "new-room".to_owned(),
            title: request.title,
            position: 0,
            locked: request.password.is_some(),
            parent: request.parent,
        })
    }

    async fn admin_update_room(
        &self,
        jwt: &str,
        id: &str,
        request: UpdateRoomRequest,
    ) -> Result<AdminRoom, SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::UpdateRoom {
            jwt: jwt.to_owned(),
            id: id.to_owned(),
            request: request.clone(),
        });
        if let Some(result) = state.admin_room_results.pop_front() {
            return result;
        }
        Ok(AdminRoom {
            id: id.to_owned(),
            title: request.title.unwrap_or_else(|| "Updated".to_owned()),
            position: request.position.unwrap_or(0),
            ..AdminRoom::default()
        })
    }

    async fn admin_delete_room(&self, jwt: &str, id: &str) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::DeleteRoom {
            jwt: jwt.to_owned(),
            id: id.to_owned(),
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_mute_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::MuteParticipant {
            jwt: jwt.to_owned(),
            room: room.to_owned(),
            identity: identity.to_owned(),
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_stop_share_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state
            .admin_calls
            .push(AdminTokenCall::StopShareParticipant {
                jwt: jwt.to_owned(),
                room: room.to_owned(),
                identity: identity.to_owned(),
            });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_kick_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::KickParticipant {
            jwt: jwt.to_owned(),
            room: room.to_owned(),
            identity: identity.to_owned(),
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_move_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
        request: MoveParticipantRequest,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::MoveParticipant {
            jwt: jwt.to_owned(),
            room: room.to_owned(),
            identity: identity.to_owned(),
            request,
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::DeleteChat {
            jwt: jwt.to_owned(),
            room: room.to_owned(),
            id: id.to_owned(),
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_rotate_invite(
        &self,
        jwt: &str,
        request: UpdateInviteRequest,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::RotateInvite {
            jwt: jwt.to_owned(),
            request,
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn upload_avatar(
        &self,
        jwt: &str,
        bytes: Vec<u8>,
    ) -> Result<AvatarUploadResponse, SessionError> {
        let mut state = lock(&self.state);
        state
            .upload_avatar_calls
            .push((jwt.to_owned(), bytes.clone()));
        if let Some(result) = state.upload_avatar_results.pop_front() {
            return result;
        }
        let version = "0123456789abcdef".to_owned();
        if let Some(client_id) = state.requests.last().map(|r| r.client_id.clone()) {
            state.avatars.insert((client_id, version.clone()), bytes);
        }
        Ok(AvatarUploadResponse { version })
    }

    async fn delete_avatar(&self, jwt: &str) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.delete_avatar_calls.push(jwt.to_owned());
        state.delete_avatar_results.pop_front().unwrap_or(Ok(()))
    }

    async fn download_avatar(
        &self,
        jwt: &str,
        identity: &str,
        version: Option<&str>,
    ) -> Result<Option<Vec<u8>>, SessionError> {
        let mut state = lock(&self.state);
        state.download_avatar_calls.push((
            jwt.to_owned(),
            identity.to_owned(),
            version.map(str::to_owned),
        ));
        let key = version.map(|v| (identity.to_owned(), v.to_owned()));
        let found = match key {
            Some(pair) => state.avatars.get(&pair).cloned(),
            None => state
                .avatars
                .iter()
                .find(|((id, _), _)| id == identity)
                .map(|(_, data)| data.clone()),
        };
        Ok(found)
    }
}
