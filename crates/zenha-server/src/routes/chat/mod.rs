//! Routes for saved chat messages per room (`/rooms/{room}/chat`).

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use zenha_protocol::{
    ChatHistoryResponse, ChatMessage, MAX_CHAT_ID_LEN, SaveChatRequest, validate_chat_text,
};

use crate::store::{DeleteOutcome, InsertOutcome};

use super::{ApiError, AppState, authenticate_room};

pub(super) fn validate_chat_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() || id.len() > MAX_CHAT_ID_LEN {
        return Err(ApiError::Invalid("invalid message id"));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    if !id.chars().all(allowed) {
        return Err(ApiError::Invalid("invalid message id"));
    }
    Ok(())
}

fn parse_before_query(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(raw) = raw.filter(|q| !q.is_empty()) else {
        return Ok(None);
    };
    let mut before: Option<String> = None;
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "before" {
            if before.is_some() {
                return Err(ApiError::Invalid("duplicate before parameter"));
            }
            validate_chat_id(value)?;
            before = Some(value.to_owned());
        }
    }
    Ok(before)
}

pub(super) async fn post_chat(
    State(state): State<AppState>,
    Path(room): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<ChatMessage>), ApiError> {
    let claims = authenticate_room(&state.config, &headers, &room)?;
    let request: SaveChatRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    validate_chat_id(&request.id)?;
    validate_chat_text(&request.text)
        .map_err(|_| ApiError::Invalid("invalid chat message text"))?;

    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    if !state.chat_limiter.check(&claims.sender_id) {
        return Err(ApiError::RateLimited);
    }

    let outcome = chat
        .insert(
            room,
            request.id,
            claims.sender_id,
            claims.sender_name,
            request.text,
            request.sent_at_unix_ms,
        )
        .await
        .map_err(|err| {
            tracing::error!(%err, "chat insert failed");
            ApiError::ChatStoreFailure
        })?;

    match outcome {
        InsertOutcome::Created(message) => Ok((StatusCode::CREATED, Json(message))),
        InsertOutcome::DuplicateBySameSender(message) => Ok((StatusCode::OK, Json(message))),
        InsertOutcome::ConflictOtherSender => Err(ApiError::MessageConflict),
    }
}

pub(super) async fn list_chat(
    State(state): State<AppState>,
    Path(room): Path<String>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ChatHistoryResponse>, ApiError> {
    let _claims = authenticate_room(&state.config, &headers, &room)?;
    let before = parse_before_query(raw_query.as_deref())?;
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;

    let history = chat.list_page(room, before).await.map_err(|err| {
        tracing::error!(%err, "chat history lookup failed");
        ApiError::ChatStoreFailure
    })?;
    Ok(Json(history))
}

pub(super) async fn delete_chat(
    State(state): State<AppState>,
    Path((room, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let claims = authenticate_room(&state.config, &headers, &room)?;
    validate_chat_id(&id)?;
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;

    match chat
        .delete(room, id, claims.sender_id)
        .await
        .map_err(|err| {
            tracing::error!(%err, "chat delete failed");
            ApiError::ChatStoreFailure
        })? {
        DeleteOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteOutcome::NotFound => Err(ApiError::MessageNotFound),
        DeleteOutcome::Forbidden => Err(ApiError::ForbiddenMessage),
    }
}

#[cfg(test)]
mod tests;
