//! HTTP router, invite/JWT authentication, API errors and shared state.

mod admin;
mod avatar;
mod chat;
mod rate_limit;
mod rooms;
#[cfg(test)]
mod test_support;
mod token;

use std::sync::Arc;
use std::time::Duration;

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use livekit_api::access_token::{Claims, TokenVerifier};
use subtle::ConstantTimeEq;
use zenha_protocol::{ErrorResponse, MAX_AVATAR_UPLOAD_BYTES, validate_room_name};

use crate::config::Config;
use crate::directory::{RoomDirectory, RoomModerator};
use crate::store::{ChatStore, RoomRecord};

pub use self::admin::DEFAULT_ADMIN_FAILURE_DELAY;
#[cfg(test)]
use self::rate_limit::AVATAR_RATE_LIMIT;
pub use self::rate_limit::{
    AdminLoginLimiter, AvatarRateLimiter, ChatRateLimiter, RoomUnlockLimiter,
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub directory: Arc<dyn RoomDirectory>,
    pub moderator: Arc<dyn RoomModerator>,
    pub chat: Option<Arc<ChatStore>>,
    pub chat_limiter: Arc<ChatRateLimiter>,
    pub avatar_limiter: Arc<AvatarRateLimiter>,
    pub admin_login_limiter: Arc<AdminLoginLimiter>,
    pub unlock_limiter: Arc<RoomUnlockLimiter>,
    pub admin_failure_delay: Duration,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/token", post(token::issue_token))
        .route("/rooms", get(rooms::list_rooms))
        .route("/rooms/{room}/unlock", post(rooms::unlock_room))
        .route(
            "/avatar",
            put(avatar::put_avatar)
                .delete(avatar::delete_avatar)
                .layer(DefaultBodyLimit::max(MAX_AVATAR_UPLOAD_BYTES)),
        )
        .route("/avatar/{identity}", get(avatar::get_avatar))
        .route(
            "/rooms/{room}/chat",
            get(chat::list_chat).post(chat::post_chat),
        )
        .route("/rooms/{room}/chat/{id}", delete(chat::delete_chat))
        .route("/admin/login", post(admin::admin_login))
        .route("/admin/rooms", post(admin::admin_create_room))
        .route(
            "/admin/rooms/{id}",
            patch(admin::admin_update_room).delete(admin::admin_delete_room),
        )
        .route(
            "/admin/rooms/{room}/participants/{identity}/mute",
            post(admin::admin_mute_participant),
        )
        .route(
            "/admin/rooms/{room}/participants/{identity}/stop-share",
            post(admin::admin_stop_share_participant),
        )
        .route(
            "/admin/rooms/{room}/participants/{identity}/kick",
            post(admin::admin_kick_participant),
        )
        .route(
            "/admin/rooms/{room}/participants/{identity}/move",
            post(admin::admin_move_participant),
        )
        .route(
            "/admin/rooms/{room}/chat/{id}",
            delete(admin::admin_delete_chat),
        )
        .route("/admin/invite", post(admin::admin_rotate_invite))
        .with_state(state)
}

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("malformed request body")]
    MalformedBody,
    #[error("invalid invite code")]
    Unauthorized,
    #[error("invalid admin password")]
    InvalidAdminPassword,
    #[error("invalid or missing token")]
    InvalidToken,
    #[error("admin privileges required")]
    ForbiddenAdmin,
    #[error("token does not grant access to this room")]
    ForbiddenRoom,
    #[error("room_locked")]
    RoomLocked,
    #[error("invalid room password")]
    WrongRoomPassword,
    #[error("cannot delete another user's message")]
    ForbiddenMessage,
    #[error("not found")]
    AdminNotConfigured,
    #[error("room not found")]
    RoomNotFound,
    #[error("room or participant not found")]
    TargetNotFound,
    #[error("message not found")]
    MessageNotFound,
    #[error("avatar not found")]
    AvatarNotFound,
    #[error("message id already exists")]
    MessageConflict,
    #[error("maximum room limit reached")]
    RoomLimitReached,
    #[error("cannot delete the last room")]
    CannotDeleteLastRoom,
    #[error("image exceeds upload limit")]
    PayloadTooLarge,
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("{0}")]
    InvalidAvatar(String),
    #[error("could not issue token")]
    TokenFailure,
    #[error("room directory unavailable")]
    DirectoryFailure,
    #[error("livekit moderation failed")]
    ModerationFailure,
    #[error("storage unavailable")]
    ChatUnavailable,
    #[error("storage failure")]
    ChatStoreFailure,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::MalformedBody | Self::Invalid(_) | Self::InvalidAvatar(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::Unauthorized | Self::InvalidAdminPassword | Self::InvalidToken => {
                StatusCode::UNAUTHORIZED
            }
            Self::ForbiddenAdmin
            | Self::ForbiddenRoom
            | Self::ForbiddenMessage
            | Self::RoomLocked
            | Self::WrongRoomPassword => StatusCode::FORBIDDEN,
            Self::AdminNotConfigured
            | Self::RoomNotFound
            | Self::TargetNotFound
            | Self::MessageNotFound
            | Self::AvatarNotFound => StatusCode::NOT_FOUND,
            Self::MessageConflict | Self::RoomLimitReached | Self::CannotDeleteLastRoom => {
                StatusCode::CONFLICT
            }
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::TokenFailure | Self::ChatStoreFailure => StatusCode::INTERNAL_SERVER_ERROR,
            Self::DirectoryFailure | Self::ModerationFailure => StatusCode::BAD_GATEWAY,
            Self::ChatUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        let body = ErrorResponse {
            error: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

async fn health() -> &'static str {
    "ok"
}

struct RoomClaims {
    sender_id: String,
    sender_name: String,
}

fn verify_bearer_claims(config: &Config, headers: &HeaderMap) -> Result<Claims, ApiError> {
    let header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::InvalidToken)?;
    let (scheme, token) = header.split_once(' ').ok_or(ApiError::InvalidToken)?;
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return Err(ApiError::InvalidToken);
    }
    let claims = TokenVerifier::with_api_key(&config.livekit_api_key, &config.livekit_api_secret)
        .verify(token)
        .map_err(|_| ApiError::InvalidToken)?;
    if claims.sub.trim().is_empty() {
        return Err(ApiError::InvalidToken);
    }
    Ok(claims)
}

fn authenticate_room(
    config: &Config,
    headers: &HeaderMap,
    room: &str,
) -> Result<RoomClaims, ApiError> {
    let claims = verify_bearer_claims(config, headers)?;

    validate_room_name(room).map_err(|_| ApiError::Invalid("invalid room name"))?;
    if !claims.video.room_join || claims.video.room != room {
        return Err(ApiError::ForbiddenRoom);
    }

    Ok(RoomClaims {
        sender_id: claims.sub,
        sender_name: claims.name,
    })
}

fn authenticate_admin(config: &Config, headers: &HeaderMap) -> Result<String, ApiError> {
    if config.admin_code_hash.is_none() {
        return Err(ApiError::AdminNotConfigured);
    }
    let claims = verify_bearer_claims(config, headers)?;
    if !crate::token::is_admin_claims(&claims) {
        return Err(ApiError::ForbiddenAdmin);
    }
    Ok(claims.sub)
}

/// Rooms in list order; without a database, they come from `ZENHA_ROOM_LIST`, with no password or parent room.
async fn room_records(state: &AppState) -> Result<Vec<RoomRecord>, ApiError> {
    match &state.chat {
        Some(chat) => chat.room_records().await.map_err(|err| {
            tracing::error!(%err, "room list lookup failed");
            ApiError::ChatStoreFailure
        }),
        None => Ok(state
            .config
            .rooms
            .iter()
            .enumerate()
            .map(|(position, name)| RoomRecord {
                id: name.clone(),
                title: name.clone(),
                position,
                parent: None,
                password_hash: None,
            })
            .collect()),
    }
}

async fn verify_invite(state: &AppState, provided: &str) -> Result<bool, ApiError> {
    if let Some(chat) = &state.chat {
        let runtime_hash = chat.get_invite_code_hash().await.map_err(|err| {
            tracing::error!(%err, "reading runtime invite hash failed");
            ApiError::ChatStoreFailure
        })?;
        if let Some(hash) = runtime_hash {
            return Ok(verify_password_hash(hash, provided.to_owned()).await);
        }
    }

    if let Some(hash) = &state.config.invite_code_hash {
        Ok(verify_password_hash(hash.clone(), provided.to_owned()).await)
    } else {
        Ok(invite_matches(&state.config.invite_code, provided))
    }
}

async fn verify_password_hash(hash: String, candidate: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password_hash_sync(&hash, &candidate))
        .await
        .unwrap_or(false)
}

fn verify_password_hash_sync(hash: &str, candidate: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok()
    })
}

fn invite_matches(expected: &str, provided: &str) -> bool {
    let expected = expected.as_bytes();
    let provided = provided.as_bytes();
    let mut diff = expected.len() ^ provided.len();
    // Loop length follows the attacker-supplied input only, never the secret.
    for i in 0..expected.len().max(provided.len()) {
        let a = expected.get(i).copied().unwrap_or(0);
        let b = provided.get(i).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff.ct_eq(&0).into()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use zenha_protocol::ROOM_LOCKED_ERROR;

    use super::test_support::app;
    use super::{ApiError, invite_matches};

    #[test]
    fn room_locked_error_text_matches_the_protocol() {
        assert_eq!(ApiError::RoomLocked.to_string(), ROOM_LOCKED_ERROR);
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn invite_comparison() {
        assert!(invite_matches("abc", "abc"));
        assert!(!invite_matches("abc", "abd"));
        assert!(!invite_matches("abc", "ab"));
        assert!(!invite_matches("abc", "abc\0"));
        assert!(!invite_matches("abc", ""));
    }
}
