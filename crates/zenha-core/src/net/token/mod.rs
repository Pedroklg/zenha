//! Token server HTTP client: rooms, chat, administration and avatars.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{StatusCode, header};
use zenha_protocol::{
    AdminLoginRequest, AdminLoginResponse, AdminRoom, AvatarUploadResponse, ChatHistoryResponse,
    ChatMessage, CreateRoomRequest, MoveParticipantRequest, RoomSummary, RoomsResponse,
    SaveChatRequest, TokenRequest, TokenResponse, UnlockRoomRequest, UnlockRoomResponse,
    UpdateInviteRequest, UpdateRoomRequest, validate_room_name,
};

use crate::api::SessionError;
use errors::{
    avatar_status_error, read_error_message, send_admin_request, send_avatar_request,
    send_chat_request, status_error, token_status_error, transport_error, unlock_status_error,
};

mod errors;
mod requests;
#[cfg(test)]
mod tests;
mod unavailable;

pub(crate) use unavailable::UnavailableTokenClient;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const INVITE_HEADER: &str = "x-invite-code";
const MAX_AVATAR_DOWNLOAD_BYTES: usize = 512 * 1024;

#[async_trait]
pub trait TokenClient: Send + Sync + 'static {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse, SessionError>;
    async fn fetch_rooms(
        &self,
        invite_code: &str,
        room_keys: &BTreeMap<String, String>,
    ) -> Result<Vec<RoomSummary>, SessionError>;
    async fn unlock_room(
        &self,
        room: &str,
        request: UnlockRoomRequest,
    ) -> Result<UnlockRoomResponse, SessionError>;
    async fn save_chat(
        &self,
        jwt: &str,
        room: &str,
        request: SaveChatRequest,
    ) -> Result<ChatMessage, SessionError>;
    async fn fetch_chat_history(
        &self,
        jwt: &str,
        room: &str,
        before: Option<&str>,
    ) -> Result<ChatHistoryResponse, SessionError>;
    async fn delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError>;
    async fn admin_login(
        &self,
        request: AdminLoginRequest,
    ) -> Result<AdminLoginResponse, SessionError>;
    async fn admin_create_room(
        &self,
        jwt: &str,
        request: CreateRoomRequest,
    ) -> Result<AdminRoom, SessionError>;
    async fn admin_update_room(
        &self,
        jwt: &str,
        id: &str,
        request: UpdateRoomRequest,
    ) -> Result<AdminRoom, SessionError>;
    async fn admin_delete_room(&self, jwt: &str, id: &str) -> Result<(), SessionError>;
    async fn admin_mute_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError>;
    async fn admin_stop_share_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError>;
    async fn admin_kick_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError>;
    async fn admin_move_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
        request: MoveParticipantRequest,
    ) -> Result<(), SessionError>;
    async fn admin_delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError>;
    async fn admin_rotate_invite(
        &self,
        jwt: &str,
        request: UpdateInviteRequest,
    ) -> Result<(), SessionError>;
    async fn upload_avatar(
        &self,
        jwt: &str,
        bytes: Vec<u8>,
    ) -> Result<AvatarUploadResponse, SessionError>;
    async fn delete_avatar(&self, jwt: &str) -> Result<(), SessionError>;
    async fn download_avatar(
        &self,
        jwt: &str,
        identity: &str,
        version: Option<&str>,
    ) -> Result<Option<Vec<u8>>, SessionError>;
}

#[derive(Debug, Clone)]
pub struct HttpTokenClient {
    http: reqwest::Client,
    base_url: String,
}

impl HttpTokenClient {
    pub fn new(token_server_url: &str) -> Result<Self, SessionError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| SessionError::Internal(err.to_string()))?;
        Ok(Self {
            http,
            base_url: token_server_url.trim_end_matches('/').to_owned(),
        })
    }
}

#[async_trait]
impl TokenClient for HttpTokenClient {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse, SessionError> {
        let response = self
            .token_http_request(&request)
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        let status = response.status();
        if !status.is_success() {
            let body_error = read_error_message(response).await;
            return Err(token_status_error(
                status,
                body_error.as_deref(),
                &request.room,
            ));
        }
        response
            .json::<TokenResponse>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn fetch_rooms(
        &self,
        invite_code: &str,
        room_keys: &BTreeMap<String, String>,
    ) -> Result<Vec<RoomSummary>, SessionError> {
        let response = self
            .rooms_http_request(invite_code, room_keys)
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        let body = response
            .json::<RoomsResponse>()
            .await
            .map_err(|err| transport_error(&err))?;
        Ok(body.rooms)
    }

    async fn unlock_room(
        &self,
        room: &str,
        request: UnlockRoomRequest,
    ) -> Result<UnlockRoomResponse, SessionError> {
        validate_room_name(room).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        let response = self
            .http
            .post(self.url(&format!("/rooms/{room}/unlock")))
            .json(&request)
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        if !response.status().is_success() {
            return Err(unlock_status_error(response.status()));
        }
        response
            .json::<UnlockRoomResponse>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn save_chat(
        &self,
        jwt: &str,
        room: &str,
        request: SaveChatRequest,
    ) -> Result<ChatMessage, SessionError> {
        let url = self.chat_url(room)?;
        let response =
            send_chat_request(self.http.post(url).bearer_auth(jwt).json(&request)).await?;
        response
            .json::<ChatMessage>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn fetch_chat_history(
        &self,
        jwt: &str,
        room: &str,
        before: Option<&str>,
    ) -> Result<ChatHistoryResponse, SessionError> {
        let url = self.chat_history_url(room, before)?;
        let response = send_chat_request(
            self.http
                .get(url)
                .bearer_auth(jwt)
                .header(header::ACCEPT, "application/json"),
        )
        .await?;
        response
            .json::<ChatHistoryResponse>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError> {
        let url = self.chat_delete_url(room, id)?;
        let _ = send_chat_request(self.http.delete(url).bearer_auth(jwt)).await?;
        Ok(())
    }

    async fn admin_login(
        &self,
        request: AdminLoginRequest,
    ) -> Result<AdminLoginResponse, SessionError> {
        let response = send_admin_request(
            self.http.post(self.url("/admin/login")).json(&request),
            true,
            false,
        )
        .await?;
        response
            .json::<AdminLoginResponse>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn admin_create_room(
        &self,
        jwt: &str,
        request: CreateRoomRequest,
    ) -> Result<AdminRoom, SessionError> {
        let response = send_admin_request(
            self.http
                .post(self.url("/admin/rooms"))
                .bearer_auth(jwt)
                .json(&request),
            false,
            false,
        )
        .await?;
        response
            .json::<AdminRoom>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn admin_update_room(
        &self,
        jwt: &str,
        id: &str,
        request: UpdateRoomRequest,
    ) -> Result<AdminRoom, SessionError> {
        let url = self.admin_room_url(id)?;
        let response = send_admin_request(
            self.http.patch(url).bearer_auth(jwt).json(&request),
            false,
            true,
        )
        .await?;
        response
            .json::<AdminRoom>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn admin_delete_room(&self, jwt: &str, id: &str) -> Result<(), SessionError> {
        let url = self.admin_room_url(id)?;
        let _ = send_admin_request(self.http.delete(url).bearer_auth(jwt), false, true).await?;
        Ok(())
    }

    async fn admin_mute_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let url = self.admin_participant_url(room, identity, "mute")?;
        let _ = send_admin_request(self.http.post(url).bearer_auth(jwt), false, true).await?;
        Ok(())
    }

    async fn admin_stop_share_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let url = self.admin_participant_url(room, identity, "stop-share")?;
        let _ = send_admin_request(self.http.post(url).bearer_auth(jwt), false, true).await?;
        Ok(())
    }

    async fn admin_kick_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let url = self.admin_participant_url(room, identity, "kick")?;
        let _ = send_admin_request(self.http.post(url).bearer_auth(jwt), false, true).await?;
        Ok(())
    }

    async fn admin_move_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
        request: MoveParticipantRequest,
    ) -> Result<(), SessionError> {
        let url = self.admin_participant_url(room, identity, "move")?;
        let _ = send_admin_request(
            self.http.post(url).bearer_auth(jwt).json(&request),
            false,
            true,
        )
        .await?;
        Ok(())
    }

    async fn admin_delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError> {
        let url = self.admin_chat_delete_url(room, id)?;
        let _ = send_admin_request(self.http.delete(url).bearer_auth(jwt), false, true).await?;
        Ok(())
    }

    async fn admin_rotate_invite(
        &self,
        jwt: &str,
        request: UpdateInviteRequest,
    ) -> Result<(), SessionError> {
        let _ = send_admin_request(
            self.http
                .post(self.url("/admin/invite"))
                .bearer_auth(jwt)
                .json(&request),
            false,
            false,
        )
        .await?;
        Ok(())
    }

    async fn upload_avatar(
        &self,
        jwt: &str,
        bytes: Vec<u8>,
    ) -> Result<AvatarUploadResponse, SessionError> {
        let response = send_avatar_request(
            self.http
                .put(self.url("/avatar"))
                .bearer_auth(jwt)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(bytes),
        )
        .await?;
        response
            .json::<AvatarUploadResponse>()
            .await
            .map_err(|err| transport_error(&err))
    }

    async fn delete_avatar(&self, jwt: &str) -> Result<(), SessionError> {
        let _ = send_avatar_request(self.http.delete(self.url("/avatar")).bearer_auth(jwt)).await?;
        Ok(())
    }

    async fn download_avatar(
        &self,
        jwt: &str,
        identity: &str,
        version: Option<&str>,
    ) -> Result<Option<Vec<u8>>, SessionError> {
        let url = self.avatar_get_url(identity, version)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(jwt)
            .header(header::ACCEPT, "image/png")
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body_error = read_error_message(response).await;
            return Err(avatar_status_error(status, body_error.as_deref()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_AVATAR_DOWNLOAD_BYTES as u64)
        {
            return Err(SessionError::Avatar("avatar response too large".to_owned()));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|err| transport_error(&err))?;
        if bytes.len() > MAX_AVATAR_DOWNLOAD_BYTES {
            return Err(SessionError::Avatar("avatar response too large".to_owned()));
        }
        Ok(Some(bytes.to_vec()))
    }
}
