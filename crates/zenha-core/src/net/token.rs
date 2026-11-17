use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{StatusCode, header};
use zenha_protocol::{
    AdminLoginRequest, AdminLoginResponse, AdminRoom, AvatarUploadResponse, ChatHistoryResponse,
    ChatMessage, CreateRoomRequest, ErrorResponse, MoveParticipantRequest, ROOM_KEYS_HEADER,
    ROOM_LOCKED_ERROR, RoomSummary, RoomsResponse, SaveChatRequest, TokenRequest, TokenResponse,
    UnlockRoomRequest, UnlockRoomResponse, UpdateInviteRequest, UpdateRoomRequest,
    encode_room_keys, validate_avatar_version, validate_client_id, validate_room_name,
};

use crate::api::SessionError;
use crate::chat::validate_chat_id;

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

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn token_http_request(&self, request: &TokenRequest) -> reqwest::RequestBuilder {
        self.http.post(self.url("/token")).json(request)
    }

    fn rooms_http_request(
        &self,
        invite_code: &str,
        room_keys: &BTreeMap<String, String>,
    ) -> reqwest::RequestBuilder {
        let request = self
            .http
            .get(self.url("/rooms"))
            .header(INVITE_HEADER, invite_code)
            .header(header::ACCEPT, "application/json");
        if room_keys.is_empty() {
            return request;
        }
        let keys = encode_room_keys(room_keys.iter().map(|(r, k)| (r.as_str(), k.as_str())));
        request.header(ROOM_KEYS_HEADER, keys)
    }

    fn chat_url(&self, room: &str) -> Result<String, SessionError> {
        validate_room_name(room).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        Ok(self.url(&format!("/rooms/{room}/chat")))
    }

    fn chat_history_url(&self, room: &str, before: Option<&str>) -> Result<String, SessionError> {
        let base = self.chat_url(room)?;
        match before {
            Some(cursor) => {
                validate_chat_id(cursor)?;
                Ok(format!("{base}?before={cursor}"))
            }
            None => Ok(base),
        }
    }

    fn chat_delete_url(&self, room: &str, id: &str) -> Result<String, SessionError> {
        let base = self.chat_url(room)?;
        validate_chat_id(id)?;
        Ok(format!("{base}/{id}"))
    }

    fn admin_room_url(&self, id: &str) -> Result<String, SessionError> {
        validate_room_name(id).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        Ok(self.url(&format!("/admin/rooms/{id}")))
    }

    fn admin_participant_url(
        &self,
        room: &str,
        identity: &str,
        action: &str,
    ) -> Result<String, SessionError> {
        validate_room_name(room).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        validate_client_id(identity).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        Ok(self.url(&format!(
            "/admin/rooms/{room}/participants/{identity}/{action}"
        )))
    }

    fn admin_chat_delete_url(&self, room: &str, id: &str) -> Result<String, SessionError> {
        validate_room_name(room).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        validate_chat_id(id)?;
        Ok(self.url(&format!("/admin/rooms/{room}/chat/{id}")))
    }

    fn avatar_get_url(
        &self,
        identity: &str,
        version: Option<&str>,
    ) -> Result<String, SessionError> {
        validate_client_id(identity).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
        let base = self.url(&format!("/avatar/{identity}"));
        match version {
            Some(version) => {
                validate_avatar_version(version)
                    .map_err(|err| SessionError::InvalidInput(err.to_string()))?;
                Ok(format!("{base}?v={version}"))
            }
            None => Ok(base),
        }
    }
}

fn transport_error(err: &reqwest::Error) -> SessionError {
    let message = if err.is_timeout() {
        "request timed out"
    } else if err.is_connect() {
        "connection failed"
    } else if err.is_decode() {
        "invalid response"
    } else {
        "request failed"
    };
    SessionError::TokenServer(message.to_owned())
}

fn status_error(status: StatusCode) -> SessionError {
    if status == StatusCode::UNAUTHORIZED {
        SessionError::InviteRejected
    } else {
        SessionError::TokenServer(format!("server returned HTTP {}", status.as_u16()))
    }
}

fn token_status_error(status: StatusCode, body_error: Option<&str>, room: &str) -> SessionError {
    if status == StatusCode::FORBIDDEN && body_error == Some(ROOM_LOCKED_ERROR) {
        SessionError::RoomLocked(room.to_owned())
    } else {
        status_error(status)
    }
}

fn unlock_status_error(status: StatusCode) -> SessionError {
    match status {
        StatusCode::FORBIDDEN => SessionError::RoomPasswordRejected,
        StatusCode::TOO_MANY_REQUESTS => SessionError::RoomUnlockRateLimited,
        StatusCode::NOT_FOUND => SessionError::TokenServer("room not found".to_owned()),
        StatusCode::BAD_REQUEST => SessionError::TokenServer("room has no password".to_owned()),
        other => status_error(other),
    }
}

fn chat_status_error(status: StatusCode) -> SessionError {
    match status {
        StatusCode::UNAUTHORIZED => SessionError::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => SessionError::RateLimited,
        StatusCode::FORBIDDEN => {
            SessionError::TokenServer("not allowed for this room or message".to_owned())
        }
        StatusCode::NOT_FOUND => SessionError::TokenServer("message not found".to_owned()),
        StatusCode::CONFLICT => SessionError::TokenServer("message id conflict".to_owned()),
        StatusCode::SERVICE_UNAVAILABLE => {
            SessionError::TokenServer("chat history unavailable on server".to_owned())
        }
        StatusCode::BAD_REQUEST => SessionError::TokenServer("invalid chat request".to_owned()),
        other => SessionError::TokenServer(format!("server returned HTTP {}", other.as_u16())),
    }
}

async fn send_chat_request(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, SessionError> {
    let response = request.send().await.map_err(|err| transport_error(&err))?;
    if !response.status().is_success() {
        return Err(chat_status_error(response.status()));
    }
    Ok(response)
}

fn avatar_status_error(status: StatusCode, body_error: Option<&str>) -> SessionError {
    match status {
        StatusCode::UNAUTHORIZED => SessionError::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => SessionError::RateLimited,
        StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE => {
            let fallback = if status == StatusCode::PAYLOAD_TOO_LARGE {
                "image exceeds 2 MB limit"
            } else {
                "invalid avatar image"
            };
            SessionError::Avatar(non_empty_error(body_error, fallback))
        }
        StatusCode::SERVICE_UNAVAILABLE => {
            SessionError::Avatar("avatar storage unavailable on server".to_owned())
        }
        other => SessionError::TokenServer(format!("server returned HTTP {}", other.as_u16())),
    }
}

async fn send_avatar_request(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, SessionError> {
    let response = request.send().await.map_err(|err| transport_error(&err))?;
    let status = response.status();
    if !status.is_success() {
        let body_error = read_error_message(response).await;
        return Err(avatar_status_error(status, body_error.as_deref()));
    }
    Ok(response)
}

fn non_empty_error(body_error: Option<&str>, fallback: &str) -> String {
    body_error
        .map(str::trim)
        .filter(|msg| !msg.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

fn admin_login_error(status: StatusCode, body_error: Option<&str>) -> SessionError {
    match status {
        StatusCode::UNAUTHORIZED => SessionError::AdminInvalidPassword,
        StatusCode::NOT_FOUND => SessionError::AdminDisabled,
        StatusCode::TOO_MANY_REQUESTS => SessionError::AdminRateLimited,
        StatusCode::BAD_REQUEST => {
            SessionError::InvalidInput(non_empty_error(body_error, "invalid admin login request"))
        }
        other => {
            let fallback = format!("server returned HTTP {}", other.as_u16());
            SessionError::Admin(non_empty_error(body_error, &fallback))
        }
    }
}

fn admin_status_error(
    status: StatusCode,
    body_error: Option<&str>,
    has_target: bool,
) -> SessionError {
    match status {
        StatusCode::UNAUTHORIZED => SessionError::AdminUnauthorized,
        StatusCode::FORBIDDEN => SessionError::AdminForbidden,
        StatusCode::TOO_MANY_REQUESTS => SessionError::AdminRateLimited,
        StatusCode::NOT_FOUND => {
            let trimmed = body_error.map(str::trim).filter(|s| !s.is_empty());
            if !has_target || trimmed == Some("not found") {
                SessionError::AdminDisabled
            } else {
                SessionError::AdminTargetNotFound(non_empty_error(trimmed, "target not found"))
            }
        }
        StatusCode::BAD_REQUEST => {
            SessionError::InvalidInput(non_empty_error(body_error, "invalid admin request"))
        }
        other => {
            let fallback = format!("server returned HTTP {}", other.as_u16());
            SessionError::Admin(non_empty_error(body_error, &fallback))
        }
    }
}

async fn read_error_message(response: reqwest::Response) -> Option<String> {
    response
        .json::<ErrorResponse>()
        .await
        .ok()
        .map(|err| err.error)
}

async fn send_admin_request(
    request: reqwest::RequestBuilder,
    is_login: bool,
    has_target: bool,
) -> Result<reqwest::Response, SessionError> {
    let response = request.send().await.map_err(|err| transport_error(&err))?;
    let status = response.status();
    if !status.is_success() {
        let body_error = read_error_message(response).await;
        return Err(if is_login {
            admin_login_error(status, body_error.as_deref())
        } else {
            admin_status_error(status, body_error.as_deref(), has_target)
        });
    }
    Ok(response)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_keys_go_in_the_rooms_header_and_the_token_body() {
        let client = HttpTokenClient::new("http://server.invalid/").unwrap();
        let keys = BTreeMap::from([
            ("geral".to_owned(), "k1".to_owned()),
            ("vip".to_owned(), "k2".to_owned()),
        ]);
        let rooms = client.rooms_http_request("convite", &keys).build().unwrap();
        assert_eq!(rooms.url().as_str(), "http://server.invalid/rooms");
        assert_eq!(rooms.headers()[ROOM_KEYS_HEADER], "geral=k1,vip=k2");
        assert_eq!(rooms.headers()[INVITE_HEADER], "convite");
        let plain = client
            .rooms_http_request("convite", &BTreeMap::new())
            .build()
            .unwrap();
        assert!(!plain.headers().contains_key(ROOM_KEYS_HEADER));

        let token = client
            .token_http_request(&TokenRequest {
                client_id: "client-pedro".to_owned(),
                display_name: "Pedro".to_owned(),
                room: "vip".to_owned(),
                invite_code: "convite".to_owned(),
                room_key: Some("k2".to_owned()),
            })
            .build()
            .unwrap();
        let body = token.body().and_then(reqwest::Body::as_bytes).unwrap();
        let sent: TokenRequest = serde_json::from_slice(body).unwrap();
        assert_eq!(sent.room_key.as_deref(), Some("k2"));
    }

    #[test]
    fn room_lock_status_mapping() {
        assert_eq!(
            token_status_error(StatusCode::FORBIDDEN, Some(ROOM_LOCKED_ERROR), "vip"),
            SessionError::RoomLocked("vip".to_owned())
        );
        assert!(matches!(
            token_status_error(StatusCode::FORBIDDEN, Some("other"), "vip"),
            SessionError::TokenServer(_)
        ));
        assert_eq!(
            unlock_status_error(StatusCode::FORBIDDEN),
            SessionError::RoomPasswordRejected
        );
        assert_eq!(
            unlock_status_error(StatusCode::TOO_MANY_REQUESTS),
            SessionError::RoomUnlockRateLimited
        );
        assert_eq!(
            unlock_status_error(StatusCode::UNAUTHORIZED),
            SessionError::InviteRejected
        );
    }

    #[test]
    fn status_mapping() {
        assert_eq!(
            status_error(StatusCode::UNAUTHORIZED),
            SessionError::InviteRejected
        );
        assert_eq!(
            status_error(StatusCode::INTERNAL_SERVER_ERROR),
            SessionError::TokenServer("server returned HTTP 500".to_owned())
        );
    }

    #[test]
    fn chat_status_mapping() {
        assert_eq!(
            chat_status_error(StatusCode::UNAUTHORIZED),
            SessionError::Unauthorized
        );
        let rate_limited = chat_status_error(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rate_limited, SessionError::RateLimited);
        assert!(!rate_limited.to_string().contains("429"));
        assert!(matches!(
            chat_status_error(StatusCode::FORBIDDEN),
            SessionError::TokenServer(_)
        ));
        assert!(matches!(
            chat_status_error(StatusCode::NOT_FOUND),
            SessionError::TokenServer(_)
        ));
        assert!(matches!(
            chat_status_error(StatusCode::CONFLICT),
            SessionError::TokenServer(_)
        ));
        assert!(matches!(
            chat_status_error(StatusCode::SERVICE_UNAVAILABLE),
            SessionError::TokenServer(_)
        ));
        assert_eq!(
            chat_status_error(StatusCode::INTERNAL_SERVER_ERROR),
            SessionError::TokenServer("server returned HTTP 500".to_owned())
        );
    }

    #[test]
    fn url_joins_without_double_slash() {
        let client = HttpTokenClient::new("http://host:8080/").map(|c| c.url("/token"));
        assert_eq!(client.as_deref(), Ok("http://host:8080/token"));
    }

    #[test]
    fn chat_urls_build_and_validate() {
        let Ok(client) = HttpTokenClient::new("http://host:8080/") else {
            panic!("valid url should construct client");
        };
        assert_eq!(
            client.chat_url("geral").as_deref(),
            Ok("http://host:8080/rooms/geral/chat")
        );
        assert_eq!(
            client.chat_history_url("geral", None).as_deref(),
            Ok("http://host:8080/rooms/geral/chat")
        );
        assert_eq!(
            client.chat_history_url("geral", Some("msg_9-a")).as_deref(),
            Ok("http://host:8080/rooms/geral/chat?before=msg_9-a")
        );
        assert_eq!(
            client.chat_delete_url("geral", "msg_9-a").as_deref(),
            Ok("http://host:8080/rooms/geral/chat/msg_9-a")
        );
        assert!(client.chat_url("../bad").is_err());
        assert!(client.chat_history_url("geral", Some("a&b")).is_err());
        assert!(client.chat_delete_url("geral", "").is_err());
    }

    #[test]
    fn admin_login_status_mapping_distinguishes_errors() {
        assert_eq!(
            admin_login_error(StatusCode::UNAUTHORIZED, Some("invalid admin password")),
            SessionError::AdminInvalidPassword
        );
        assert_eq!(
            admin_login_error(StatusCode::NOT_FOUND, Some("not found")),
            SessionError::AdminDisabled
        );
        assert_eq!(
            admin_login_error(StatusCode::TOO_MANY_REQUESTS, Some("rate limit exceeded")),
            SessionError::AdminRateLimited
        );
        assert_eq!(
            admin_login_error(StatusCode::BAD_REQUEST, Some("invalid client_id")),
            SessionError::InvalidInput("invalid client_id".to_owned())
        );
        assert_eq!(
            admin_login_error(StatusCode::INTERNAL_SERVER_ERROR, None),
            SessionError::Admin("server returned HTTP 500".to_owned())
        );
    }

    #[test]
    fn admin_action_status_mapping_distinguishes_404_401_403_429() {
        assert_eq!(
            admin_status_error(
                StatusCode::UNAUTHORIZED,
                Some("invalid or missing token"),
                true
            ),
            SessionError::AdminUnauthorized
        );
        assert_eq!(
            admin_status_error(
                StatusCode::FORBIDDEN,
                Some("admin privileges required"),
                true
            ),
            SessionError::AdminForbidden
        );
        assert_eq!(
            admin_status_error(StatusCode::TOO_MANY_REQUESTS, None, false),
            SessionError::AdminRateLimited
        );
        assert_eq!(
            admin_status_error(StatusCode::NOT_FOUND, Some("not found"), true),
            SessionError::AdminDisabled
        );
        assert_eq!(
            admin_status_error(StatusCode::NOT_FOUND, None, false),
            SessionError::AdminDisabled
        );
        assert_eq!(
            admin_status_error(
                StatusCode::NOT_FOUND,
                Some("room or participant not found"),
                true
            ),
            SessionError::AdminTargetNotFound("room or participant not found".to_owned())
        );
        assert_eq!(
            admin_status_error(
                StatusCode::CONFLICT,
                Some("cannot delete the last room"),
                true
            ),
            SessionError::Admin("cannot delete the last room".to_owned())
        );
    }

    #[test]
    fn admin_urls_build_and_validate() {
        let Ok(client) = HttpTokenClient::new("http://host:8080/") else {
            panic!("valid url should construct client");
        };
        assert_eq!(
            client.admin_room_url("geral").as_deref(),
            Ok("http://host:8080/admin/rooms/geral")
        );
        assert_eq!(
            client
                .admin_participant_url("geral", "ana-1", "kick")
                .as_deref(),
            Ok("http://host:8080/admin/rooms/geral/participants/ana-1/kick")
        );
        assert_eq!(
            client.admin_chat_delete_url("geral", "msg_1").as_deref(),
            Ok("http://host:8080/admin/rooms/geral/chat/msg_1")
        );
        assert!(client.admin_room_url("../bad").is_err());
        assert!(
            client
                .admin_participant_url("geral", "../bad", "kick")
                .is_err()
        );
        assert!(client.admin_chat_delete_url("geral", "a/b").is_err());
    }

    #[test]
    fn avatar_urls_and_status_errors_validate() {
        let client = HttpTokenClient::new("http://host:8080/").unwrap();
        assert_eq!(
            client
                .avatar_get_url("client-ana", Some("0123456789abcdef"))
                .as_deref(),
            Ok("http://host:8080/avatar/client-ana?v=0123456789abcdef")
        );
        assert_eq!(
            client.avatar_get_url("client-ana", None).as_deref(),
            Ok("http://host:8080/avatar/client-ana")
        );
        assert!(client.avatar_get_url("../bad", None).is_err());
        assert!(client.avatar_get_url("client-ana", Some("../bad")).is_err());

        assert_eq!(
            avatar_status_error(StatusCode::UNAUTHORIZED, None),
            SessionError::Unauthorized
        );
        assert_eq!(
            avatar_status_error(StatusCode::TOO_MANY_REQUESTS, None),
            SessionError::RateLimited
        );
        assert_eq!(
            avatar_status_error(StatusCode::PAYLOAD_TOO_LARGE, Some("too big")),
            SessionError::Avatar("too big".to_owned())
        );
        assert_eq!(
            avatar_status_error(StatusCode::BAD_REQUEST, Some("bad format")),
            SessionError::Avatar("bad format".to_owned())
        );
    }
}
