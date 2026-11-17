//! Token client used before sign-in: every call fails with "not signed in".

use std::collections::BTreeMap;

use async_trait::async_trait;
use zenha_protocol::{
    AdminLoginRequest, AdminLoginResponse, AdminRoom, AvatarUploadResponse, ChatHistoryResponse,
    ChatMessage, CreateRoomRequest, MoveParticipantRequest, RoomSummary, SaveChatRequest,
    TokenRequest, TokenResponse, UnlockRoomRequest, UnlockRoomResponse, UpdateInviteRequest,
    UpdateRoomRequest,
};

use super::TokenClient;
use crate::api::SessionError;

#[derive(Debug, Clone, Copy)]
pub(crate) struct UnavailableTokenClient;

#[async_trait]
impl TokenClient for UnavailableTokenClient {
    async fn fetch_token(&self, _request: TokenRequest) -> Result<TokenResponse, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn fetch_rooms(
        &self,
        _invite_code: &str,
        _room_keys: &BTreeMap<String, String>,
    ) -> Result<Vec<RoomSummary>, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn unlock_room(
        &self,
        _room: &str,
        _request: UnlockRoomRequest,
    ) -> Result<UnlockRoomResponse, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn save_chat(
        &self,
        _jwt: &str,
        _room: &str,
        _request: SaveChatRequest,
    ) -> Result<ChatMessage, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn fetch_chat_history(
        &self,
        _jwt: &str,
        _room: &str,
        _before: Option<&str>,
    ) -> Result<ChatHistoryResponse, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn delete_chat(&self, _jwt: &str, _room: &str, _id: &str) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_login(
        &self,
        _request: AdminLoginRequest,
    ) -> Result<AdminLoginResponse, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_create_room(
        &self,
        _jwt: &str,
        _request: CreateRoomRequest,
    ) -> Result<AdminRoom, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_update_room(
        &self,
        _jwt: &str,
        _id: &str,
        _request: UpdateRoomRequest,
    ) -> Result<AdminRoom, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_delete_room(&self, _jwt: &str, _id: &str) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_mute_participant(
        &self,
        _jwt: &str,
        _room: &str,
        _identity: &str,
    ) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_stop_share_participant(
        &self,
        _jwt: &str,
        _room: &str,
        _identity: &str,
    ) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_kick_participant(
        &self,
        _jwt: &str,
        _room: &str,
        _identity: &str,
    ) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_move_participant(
        &self,
        _jwt: &str,
        _room: &str,
        _identity: &str,
        _request: MoveParticipantRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_delete_chat(
        &self,
        _jwt: &str,
        _room: &str,
        _id: &str,
    ) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn admin_rotate_invite(
        &self,
        _jwt: &str,
        _request: UpdateInviteRequest,
    ) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn upload_avatar(
        &self,
        _jwt: &str,
        _bytes: Vec<u8>,
    ) -> Result<AvatarUploadResponse, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn delete_avatar(&self, _jwt: &str) -> Result<(), SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }

    async fn download_avatar(
        &self,
        _jwt: &str,
        _identity: &str,
        _version: Option<&str>,
    ) -> Result<Option<Vec<u8>>, SessionError> {
        Err(SessionError::TokenServer("not signed in".to_owned()))
    }
}
