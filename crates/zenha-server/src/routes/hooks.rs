//! Public incoming webhooks: Discord-format body in, `WebhookPost` to everyone in the room out.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;
use uuid::Uuid;
use zenha_protocol::{
    DiscordWebhookBody, PersonalHookRequest, PersonalHookResponse, WEBHOOK_TOPIC,
    validate_client_id, validate_webhook_id,
};

use crate::directory::ModerationError;
use crate::room_access::hmac_sha256;
use crate::store::{now_unix_ms, token_hash};

use super::{ApiError, AppState, room_records, verify_invite};

const PREFLIGHT_MAX_AGE_SECS: &str = "86400";
const PERSONAL_HOOK_SENDER_NAME: &str = "Gamers Club";
const PERSONAL_HOOK_LABEL: &[u8] = b"zenha-personal-hook:";
const PERSONAL_HOOK_SIG_BYTES: usize = 16;

pub(super) fn personal_hook_sig(secret: &str, identity: &str) -> String {
    let mut message = Vec::with_capacity(PERSONAL_HOOK_LABEL.len() + identity.len());
    message.extend_from_slice(PERSONAL_HOOK_LABEL);
    message.extend_from_slice(identity.as_bytes());
    let mac = hmac_sha256(secret.as_bytes(), &message);
    mac[..PERSONAL_HOOK_SIG_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn personal_hook_sig_matches(secret: &str, identity: &str, provided: &str) -> bool {
    personal_hook_sig(secret, identity)
        .as_bytes()
        .ct_eq(provided.as_bytes())
        .into()
}

pub(super) async fn issue_personal_hook(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<PersonalHookResponse>, ApiError> {
    let request: PersonalHookRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    if !verify_invite(&state, &request.invite_code).await? {
        return Err(ApiError::Unauthorized);
    }
    validate_client_id(&request.client_id).map_err(|_| ApiError::Invalid("invalid client id"))?;
    let sig = personal_hook_sig(&state.config.livekit_api_secret, &request.client_id);
    Ok(Json(PersonalHookResponse {
        path: format!("/hooks/u/{}/{sig}", request.client_id),
    }))
}

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
    match chat
        .save_webhook_post(record.room.clone(), id.clone(), post, post_json)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(webhook = %id, "same link as the last post; skipped");
            return Ok(StatusCode::NO_CONTENT);
        }
        Err(err) => tracing::error!(webhook = %id, %err, "storing webhook post failed"),
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

pub(super) async fn post_personal_hook(
    State(state): State<AppState>,
    Path((identity, sig)): Path<(String, String)>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    validate_client_id(&identity).map_err(|_| ApiError::WebhookNotFound)?;
    if !personal_hook_sig_matches(&state.config.livekit_api_secret, &identity, &sig) {
        return Err(ApiError::WebhookNotFound);
    }
    if !state.chat_limiter.check(&format!("webhook:u:{identity}")) {
        tracing::warn!(identity = %identity, "personal webhook rate limited");
        return Err(ApiError::RateLimited);
    }

    let parsed: DiscordWebhookBody =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    let post = parsed
        .into_post(
            Uuid::new_v4().simple().to_string(),
            PERSONAL_HOOK_SENDER_NAME.to_owned(),
            now_unix_ms(),
        )
        .ok_or(ApiError::Invalid("webhook body has nothing to show"))?;

    let Some(room) = find_participant_room(&state, &identity).await? else {
        tracing::debug!(
            identity = %identity,
            "personal webhook skipped; participant not in any room"
        );
        return Ok(StatusCode::NO_CONTENT);
    };

    let post_json = serde_json::to_string(&post).map_err(|err| {
        tracing::error!(%err, "serializing personal webhook post failed");
        ApiError::ChatStoreFailure
    })?;
    let payload = post_json.clone().into_bytes();
    if let Some(chat) = &state.chat {
        match chat
            .save_personal_webhook_post(
                room.clone(),
                format!("personal:{identity}"),
                post,
                post_json,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(
                    identity = %identity,
                    room = %room,
                    "duplicate personal webhook link in room; skipped"
                );
                return Ok(StatusCode::NO_CONTENT);
            }
            Err(err) => {
                tracing::error!(identity = %identity, %err, "storing personal webhook post failed");
            }
        }
    }

    match state
        .moderator
        .send_data(&room, WEBHOOK_TOPIC, payload, Vec::new())
        .await
    {
        Ok(()) => tracing::info!(identity = %identity, room = %room, "personal webhook delivered"),
        Err(ModerationError::NotFound) => {}
        Err(ModerationError::Backend(msg)) => {
            tracing::warn!(
                identity = %identity,
                room = %room,
                error = %msg,
                "personal webhook delivery failed"
            );
            return Err(ApiError::ModerationFailure);
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn find_participant_room(
    state: &AppState,
    identity: &str,
) -> Result<Option<String>, ApiError> {
    let cached_room = state
        .occupants_cache
        .as_ref()
        .and_then(|cache| cache.room_of(identity));
    if let Some(room) = &cached_room {
        match state.moderator.ensure_participant(room, identity).await {
            Ok(()) => return Ok(Some(room.clone())),
            Err(ModerationError::NotFound) => {}
            Err(ModerationError::Backend(msg)) => {
                tracing::warn!(
                    room = %room,
                    error = %msg,
                    "personal webhook participant lookup failed"
                );
                return Err(ApiError::ModerationFailure);
            }
        }
    }

    let active_rooms = state.moderator.list_active_rooms().await.map_err(|err| {
        tracing::warn!(%err, "personal webhook active room lookup failed");
        ApiError::ModerationFailure
    })?;
    for room in active_rooms {
        if cached_room.as_deref() == Some(room.as_str()) {
            continue;
        }
        match state.moderator.ensure_participant(&room, identity).await {
            Ok(()) => return Ok(Some(room)),
            Err(ModerationError::NotFound) => {}
            Err(ModerationError::Backend(msg)) => {
                tracing::warn!(
                    room = %room,
                    error = %msg,
                    "personal webhook participant lookup failed"
                );
                return Err(ApiError::ModerationFailure);
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
