//! Tradução de respostas HTTP e falhas de transporte do servidor de tokens em `SessionError`.

use reqwest::StatusCode;
use zenha_protocol::{ErrorResponse, ROOM_LOCKED_ERROR};

use crate::api::SessionError;

pub(super) fn transport_error(err: &reqwest::Error) -> SessionError {
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

pub(super) fn status_error(status: StatusCode) -> SessionError {
    if status == StatusCode::UNAUTHORIZED {
        SessionError::InviteRejected
    } else {
        SessionError::TokenServer(format!("server returned HTTP {}", status.as_u16()))
    }
}

pub(super) fn token_status_error(
    status: StatusCode,
    body_error: Option<&str>,
    room: &str,
) -> SessionError {
    if status == StatusCode::FORBIDDEN && body_error == Some(ROOM_LOCKED_ERROR) {
        SessionError::RoomLocked(room.to_owned())
    } else {
        status_error(status)
    }
}

pub(super) fn unlock_status_error(status: StatusCode) -> SessionError {
    match status {
        StatusCode::FORBIDDEN => SessionError::RoomPasswordRejected,
        StatusCode::TOO_MANY_REQUESTS => SessionError::RoomUnlockRateLimited,
        StatusCode::NOT_FOUND => SessionError::TokenServer("room not found".to_owned()),
        StatusCode::BAD_REQUEST => SessionError::TokenServer("room has no password".to_owned()),
        other => status_error(other),
    }
}

pub(super) fn chat_status_error(status: StatusCode) -> SessionError {
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

pub(super) async fn send_chat_request(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, SessionError> {
    let response = request.send().await.map_err(|err| transport_error(&err))?;
    if !response.status().is_success() {
        return Err(chat_status_error(response.status()));
    }
    Ok(response)
}

pub(super) fn avatar_status_error(status: StatusCode, body_error: Option<&str>) -> SessionError {
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

pub(super) async fn send_avatar_request(
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

pub(super) fn non_empty_error(body_error: Option<&str>, fallback: &str) -> String {
    body_error
        .map(str::trim)
        .filter(|msg| !msg.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

pub(super) fn admin_login_error(status: StatusCode, body_error: Option<&str>) -> SessionError {
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

pub(super) fn admin_status_error(
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

pub(super) async fn read_error_message(response: reqwest::Response) -> Option<String> {
    response
        .json::<ErrorResponse>()
        .await
        .ok()
        .map(|err| err.error)
}

pub(super) async fn send_admin_request(
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
