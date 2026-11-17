//! Admin routes for incoming webhooks: list, create and revoke.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use uuid::Uuid;
use zenha_protocol::{CreateWebhookRequest, CreatedWebhook, WebhooksResponse, validate_webhook_id};

use crate::store::token_hash;

use super::super::{ApiError, AppState, authenticate_admin, room_records};

pub(in super::super) async fn admin_list_webhooks(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<WebhooksResponse>, ApiError> {
    authenticate_admin(&state.config, &headers)?;
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    let webhooks = chat.list_webhooks().await.map_err(|err| {
        tracing::error!(%err, "webhook list failed");
        ApiError::ChatStoreFailure
    })?;
    Ok(Json(WebhooksResponse { webhooks }))
}

pub(in super::super) async fn admin_create_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<CreatedWebhook>), ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    let request: CreateWebhookRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    request
        .validate()
        .map_err(|_| ApiError::Invalid("invalid webhook room or name"))?;
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    if !room_records(&state)
        .await?
        .iter()
        .any(|r| r.id == request.room)
    {
        return Err(ApiError::RoomNotFound);
    }

    let id = Uuid::new_v4().simple().to_string();
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let webhook = chat
        .create_webhook(
            id,
            request.room,
            request.name.trim().to_owned(),
            token_hash(&token),
        )
        .await
        .map_err(|err| {
            tracing::error!(%err, "webhook create failed");
            ApiError::ChatStoreFailure
        })?
        .ok_or(ApiError::WebhookLimitReached)?;

    tracing::info!(%admin_id, webhook = %webhook.id, room = %webhook.room, "admin created webhook");
    let url = format!("/hooks/{}/{token}", webhook.id);
    Ok((StatusCode::CREATED, Json(CreatedWebhook { webhook, url })))
}

pub(in super::super) async fn admin_delete_webhook(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_webhook_id(&id).map_err(|_| ApiError::Invalid("invalid webhook id"))?;
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    let deleted = chat.delete_webhook(id.clone()).await.map_err(|err| {
        tracing::error!(%err, "webhook delete failed");
        ApiError::ChatStoreFailure
    })?;
    if !deleted {
        return Err(ApiError::WebhookNotFound);
    }
    tracing::info!(%admin_id, webhook = %id, "admin deleted webhook");
    Ok(StatusCode::NO_CONTENT)
}
