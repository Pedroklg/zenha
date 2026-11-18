//! Public incoming webhook: Discord-format body in, `WebhookPost` to everyone in the room out.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;
use uuid::Uuid;
use zenha_protocol::{DiscordWebhookBody, WEBHOOK_TOPIC, validate_webhook_id};

use crate::directory::ModerationError;
use crate::store::{now_unix_ms, token_hash};

use super::{ApiError, AppState, room_records};

const PREFLIGHT_MAX_AGE_SECS: &str = "86400";

/// The secret lives in the URL and there are no cookies, so any origin may call the route.
pub(super) async fn allow_any_origin(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

pub(super) async fn hook_preflight() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_METHODS, "POST, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "content-type"),
            (header::ACCESS_CONTROL_MAX_AGE, PREFLIGHT_MAX_AGE_SECS),
        ],
    )
}

pub(super) async fn post_hook(
    State(state): State<AppState>,
    Path((id, token)): Path<(String, String)>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    validate_webhook_id(&id).map_err(|_| ApiError::WebhookNotFound)?;
    let chat = state.chat.as_ref().ok_or(ApiError::WebhookNotFound)?;
    let record = chat
        .get_webhook(id.clone())
        .await
        .map_err(|err| {
            tracing::error!(%err, "webhook lookup failed");
            ApiError::ChatStoreFailure
        })?
        .ok_or(ApiError::WebhookNotFound)?;
    let token_matches: bool = token_hash(&token)
        .as_bytes()
        .ct_eq(record.token_hash.as_bytes())
        .into();
    if !token_matches {
        return Err(ApiError::WebhookNotFound);
    }
    if !room_records(&state)
        .await?
        .iter()
        .any(|r| r.id == record.room)
    {
        return Err(ApiError::WebhookNotFound);
    }
    if !state.chat_limiter.check(&format!("webhook:{id}")) {
        tracing::warn!(webhook = %id, "webhook rate limited");
        return Err(ApiError::RateLimited);
    }

    let parsed: DiscordWebhookBody =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    let post = parsed
        .into_post(
            Uuid::new_v4().simple().to_string(),
            record.name,
            now_unix_ms(),
        )
        .ok_or(ApiError::Invalid("webhook body has nothing to show"))?;
    let post_json = serde_json::to_string(&post).map_err(|err| {
        tracing::error!(%err, "serializing webhook post failed");
        ApiError::ChatStoreFailure
    })?;
    let payload = post_json.clone().into_bytes();
    if let Err(err) = chat
        .save_webhook_post(record.room.clone(), id.clone(), post, post_json)
        .await
    {
        tracing::error!(webhook = %id, %err, "storing webhook post failed");
    }

    match state
        .moderator
        .send_data(&record.room, WEBHOOK_TOPIC, payload, Vec::new())
        .await
    {
        Ok(()) => tracing::info!(webhook = %id, room = %record.room, "webhook delivered"),
        Err(ModerationError::NotFound) => {}
        Err(ModerationError::Backend(msg)) => {
            tracing::warn!(webhook = %id, room = %record.room, error = %msg, "webhook delivery failed");
            return Err(ApiError::ModerationFailure);
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
