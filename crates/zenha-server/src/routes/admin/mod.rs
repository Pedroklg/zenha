//! Admin routes: login, room CRUD, moderation, chat deletion and invite rotation.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use zenha_protocol::{
    ADMIN_CHAT_DELETE_TOPIC, ADMIN_MOVE_TOPIC, ADMIN_STOP_SHARE_TOPIC, AdminChatDelete,
    AdminLoginRequest, AdminLoginResponse, AdminMove, AdminRoom, AdminStopShare, CreateRoomRequest,
    MoveParticipantRequest, ROOMS_CHANGED_TOPIC, RoomsChanged, UnlockRoomResponse,
    UpdateInviteRequest, UpdateRoomRequest, validate_client_id, validate_invite_code,
    validate_room_name, validate_room_password, validate_room_title,
};

use crate::directory::ModerationError;
use crate::room_access::{lock_owners, room_key};
use crate::store::{
    CreateRoomOutcome, DeleteRoomCheck, DeleteRoomOutcome, RoomUpdate, UpdateRoomOutcome,
};

use super::chat::validate_chat_id;
use super::{ApiError, AppState, authenticate_admin, room_records, verify_password_hash};

pub const DEFAULT_ADMIN_FAILURE_DELAY: Duration = Duration::from_millis(250);

pub(super) struct OptionalPeerIp(pub(super) Option<IpAddr>);

impl<S: Send + Sync> FromRequestParts<S> for OptionalPeerIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let ip = ConnectInfo::<SocketAddr>::from_request_parts(parts, state)
            .await
            .ok()
            .map(|ConnectInfo(addr)| addr.ip());
        Ok(Self(ip))
    }
}

fn login_rate_limit_keys(
    peer_ip: Option<IpAddr>,
    headers: &HeaderMap,
    client_id: &str,
) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    keys.push(format!("id:{client_id}"));
    if let Some(ip) = resolved_client_ip(peer_ip, headers) {
        keys.push(format!("ip:{ip}"));
    }
    keys
}

pub(super) fn resolved_client_ip(peer_ip: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
    match peer_ip {
        // Only trust X-Forwarded-For when the TCP peer is local reverse proxy (Caddy).
        Some(ip) if !ip.is_loopback() => Some(ip),
        Some(loopback) => forwarded_ip(headers).or(Some(loopback)),
        None => None,
    }
}

fn forwarded_ip(headers: &HeaderMap) -> Option<IpAddr> {
    let raw = headers.get("x-forwarded-for")?.to_str().ok()?;
    raw.rsplit(',')
        .map(str::trim)
        .find(|s| !s.is_empty())?
        .parse::<IpAddr>()
        .ok()
}

fn validate_room_and_identity(room: &str, identity: &str) -> Result<(), ApiError> {
    validate_room_name(room).map_err(|_| ApiError::Invalid("invalid room name"))?;
    validate_client_id(identity).map_err(|_| ApiError::Invalid("invalid participant identity"))?;
    Ok(())
}

async fn hash_room_password(password: String) -> Result<String, ApiError> {
    validate_room_password(&password).map_err(|_| ApiError::Invalid("invalid room password"))?;
    tokio::task::spawn_blocking(move || crate::config::hash_secret(&password))
        .await
        .map_err(|err| {
            tracing::error!(%err, "room password hashing task failed");
            ApiError::ChatStoreFailure
        })?
        .map_err(|err| {
            tracing::error!(%err, "room password hashing failed");
            ApiError::Invalid("invalid room password")
        })
}

fn validate_parent(parent: &str) -> Result<(), ApiError> {
    validate_room_name(parent).map_err(|_| ApiError::Invalid("invalid parent room"))
}

pub(super) async fn admin_login(
    State(state): State<AppState>,
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AdminLoginResponse>, ApiError> {
    let Some(admin_hash) = state.config.admin_code_hash.as_ref() else {
        return Err(ApiError::AdminNotConfigured);
    };
    let request: AdminLoginRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    validate_client_id(&request.client_id).map_err(|_| ApiError::Invalid("invalid client id"))?;

    let keys = login_rate_limit_keys(peer_ip, &headers, &request.client_id);
    if !state.admin_login_limiter.is_allowed(&keys) {
        tracing::warn!(client_id = %request.client_id, "admin login rate limited");
        return Err(ApiError::RateLimited);
    }

    let valid = verify_password_hash(admin_hash.clone(), request.password).await;
    if !valid {
        state.admin_login_limiter.record(&keys);
        tracing::warn!(client_id = %request.client_id, "admin login failed");
        if !state.admin_failure_delay.is_zero() {
            tokio::time::sleep(state.admin_failure_delay).await;
        }
        return Err(ApiError::InvalidAdminPassword);
    }

    state
        .admin_login_limiter
        .clear(&format!("id:{}", request.client_id));

    let (token, expires_at) =
        crate::token::mint_admin(&state.config, &request.client_id).map_err(|err| {
            tracing::error!(%err, "minting admin token failed");
            ApiError::TokenFailure
        })?;

    tracing::info!(client_id = %request.client_id, "admin logged in");
    Ok(Json(AdminLoginResponse { token, expires_at }))
}

pub(super) async fn admin_create_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<AdminRoom>), ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    let request: CreateRoomRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    validate_room_title(&request.title).map_err(|_| ApiError::Invalid("invalid room title"))?;
    let parent = request.parent.filter(|p| !p.is_empty());
    if let Some(parent) = &parent {
        validate_parent(parent)?;
    }
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    let password_hash = match request.password.filter(|p| !p.is_empty()) {
        Some(password) => Some(hash_room_password(password).await?),
        None => None,
    };

    let outcome = chat
        .create_room(request.title.trim().to_owned(), password_hash, parent)
        .await
        .map_err(|err| {
            tracing::error!(%err, "room create failed");
            ApiError::ChatStoreFailure
        })?;

    let room = match outcome {
        CreateRoomOutcome::Created(room) => room,
        CreateRoomOutcome::LimitReached => return Err(ApiError::RoomLimitReached),
        CreateRoomOutcome::InvalidParent => return Err(ApiError::Invalid("invalid parent room")),
    };

    broadcast_rooms_changed(&state).await;
    tracing::info!(%admin_id, room = %room.id, "admin created room");
    Ok((StatusCode::CREATED, Json(room)))
}

pub(super) async fn admin_update_room(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AdminRoom>, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_room_name(&id).map_err(|_| ApiError::Invalid("invalid room id"))?;
    let request: UpdateRoomRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    if request.title.is_none()
        && request.position.is_none()
        && request.password.is_none()
        && request.parent.is_none()
    {
        return Err(ApiError::Invalid("no fields to update"));
    }
    if let Some(title) = &request.title {
        validate_room_title(title).map_err(|_| ApiError::Invalid("invalid room title"))?;
    }
    let parent = request
        .parent
        .map(|p| if p.is_empty() { None } else { Some(p) });
    if let Some(Some(parent)) = &parent {
        validate_parent(parent)?;
    }
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    let password_hash = match request.password {
        None => None,
        Some(password) if password.is_empty() => Some(None),
        Some(password) => Some(Some(hash_room_password(password).await?)),
    };

    let update = RoomUpdate {
        title: request.title.map(|t| t.trim().to_owned()),
        position: request.position,
        password_hash,
        parent,
    };
    let room = match chat.update_room(id, update).await.map_err(|err| {
        tracing::error!(%err, "room update failed");
        ApiError::ChatStoreFailure
    })? {
        UpdateRoomOutcome::Updated(room) => room,
        UpdateRoomOutcome::NotFound => return Err(ApiError::RoomNotFound),
        UpdateRoomOutcome::InvalidParent => return Err(ApiError::Invalid("invalid parent room")),
    };

    broadcast_rooms_changed(&state).await;
    tracing::info!(%admin_id, room = %room.id, "admin updated room");
    Ok(Json(room))
}

pub(super) async fn admin_delete_room(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_room_name(&id).map_err(|_| ApiError::Invalid("invalid room id"))?;
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;

    match chat
        .check_can_delete_room(id.clone())
        .await
        .map_err(|err| {
            tracing::error!(%err, "room delete check failed");
            ApiError::ChatStoreFailure
        })? {
        DeleteRoomCheck::NotFound => return Err(ApiError::RoomNotFound),
        DeleteRoomCheck::LastRoom => return Err(ApiError::CannotDeleteLastRoom),
        DeleteRoomCheck::Ready => {}
    }

    match state.moderator.delete_room(&id).await {
        Ok(()) | Err(ModerationError::NotFound) => {}
        Err(ModerationError::Backend(msg)) => {
            tracing::warn!(room = %id, error = %msg, "livekit delete_room failed");
            return Err(ApiError::ModerationFailure);
        }
    }

    match chat.commit_delete_room(id.clone()).await.map_err(|err| {
        tracing::error!(%err, "room delete commit failed");
        ApiError::ChatStoreFailure
    })? {
        DeleteRoomOutcome::Deleted => {}
        DeleteRoomOutcome::NotFound => return Err(ApiError::RoomNotFound),
        DeleteRoomOutcome::LastRoom => return Err(ApiError::CannotDeleteLastRoom),
    }

    broadcast_rooms_changed(&state).await;
    tracing::info!(%admin_id, room = %id, "admin deleted room");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn admin_mute_participant(
    State(state): State<AppState>,
    Path((room, identity)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_room_and_identity(&room, &identity)?;
    map_moderation_result(
        state.moderator.mute_microphone(&room, &identity).await,
        "mute_microphone",
    )?;
    tracing::info!(%admin_id, %room, %identity, "admin muted participant microphone");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn admin_stop_share_participant(
    State(state): State<AppState>,
    Path((room, identity)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_room_and_identity(&room, &identity)?;
    map_moderation_result(
        state.moderator.mute_screen_share(&room, &identity).await,
        "mute_screen_share",
    )?;
    let payload = serde_json::to_vec(&AdminStopShare {}).map_err(map_signal_err)?;
    map_moderation_result(
        state
            .moderator
            .send_data(
                &room,
                ADMIN_STOP_SHARE_TOPIC,
                payload,
                vec![identity.clone()],
            )
            .await,
        "send_stop_share",
    )?;
    tracing::info!(%admin_id, %room, %identity, "admin stopped participant screen share");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn admin_kick_participant(
    State(state): State<AppState>,
    Path((room, identity)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_room_and_identity(&room, &identity)?;
    map_moderation_result(
        state.moderator.remove_participant(&room, &identity).await,
        "remove_participant",
    )?;
    tracing::info!(%admin_id, %room, %identity, "admin kicked participant");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn admin_move_participant(
    State(state): State<AppState>,
    Path((room, identity)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_room_and_identity(&room, &identity)?;
    let request: MoveParticipantRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    validate_room_name(&request.to_room)
        .map_err(|_| ApiError::Invalid("invalid destination room"))?;
    if request.to_room == room {
        return Err(ApiError::Invalid("destination room matches current room"));
    }

    let rooms = room_records(&state).await?;
    let target = rooms
        .iter()
        .find(|r| r.id == request.to_room)
        .ok_or(ApiError::RoomNotFound)?;
    let unlock = lock_owners(&rooms, target)
        .first()
        .map(|(owner, hash)| UnlockRoomResponse {
            room: (*owner).to_owned(),
            room_key: room_key(&state.config.livekit_api_secret, owner, hash),
        });

    map_moderation_result(
        state.moderator.ensure_participant(&room, &identity).await,
        "ensure_participant",
    )?;

    let payload = serde_json::to_vec(&AdminMove {
        to_room: request.to_room.clone(),
        unlock,
    })
    .map_err(map_signal_err)?;
    map_moderation_result(
        state
            .moderator
            .send_data(&room, ADMIN_MOVE_TOPIC, payload, vec![identity.clone()])
            .await,
        "send_move",
    )?;
    tracing::info!(%admin_id, from_room = %room, to_room = %request.to_room, %identity, "admin requested participant move");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn admin_delete_chat(
    State(state): State<AppState>,
    Path((room, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_room_name(&room).map_err(|_| ApiError::Invalid("invalid room name"))?;
    validate_chat_id(&id)?;
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;

    let deleted = chat
        .delete_as_admin(room.clone(), id.clone())
        .await
        .map_err(|err| {
            tracing::error!(%err, "admin chat delete failed");
            ApiError::ChatStoreFailure
        })?;
    if !deleted {
        return Err(ApiError::MessageNotFound);
    }

    let payload =
        serde_json::to_vec(&AdminChatDelete { id: id.clone() }).map_err(map_signal_err)?;
    match state
        .moderator
        .send_data(&room, ADMIN_CHAT_DELETE_TOPIC, payload, Vec::new())
        .await
    {
        Ok(()) | Err(ModerationError::NotFound) => {}
        Err(ModerationError::Backend(msg)) => {
            tracing::warn!(%room, %id, error = %msg, "admin.chat_delete broadcast failed");
        }
    }

    tracing::info!(%admin_id, %room, %id, "admin deleted chat message");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn admin_rotate_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    let request: UpdateInviteRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    validate_invite_code(&request.new_code)
        .map_err(|_| ApiError::Invalid("invalid invite code"))?;
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;

    let new_code = request.new_code;
    let hash = tokio::task::spawn_blocking(move || crate::config::hash_secret(&new_code))
        .await
        .map_err(|err| {
            tracing::error!(%err, "invite hashing task failed");
            ApiError::ChatStoreFailure
        })?
        .map_err(|err| {
            tracing::error!(%err, "invite hashing failed");
            ApiError::ChatStoreFailure
        })?;

    chat.set_invite_code_hash(hash).await.map_err(|err| {
        tracing::error!(%err, "storing invite hash failed");
        ApiError::ChatStoreFailure
    })?;

    tracing::info!(%admin_id, "admin rotated invite code");
    Ok(StatusCode::NO_CONTENT)
}

fn map_signal_err(err: serde_json::Error) -> ApiError {
    tracing::error!(%err, "serializing data signal failed");
    ApiError::ChatStoreFailure
}

fn map_moderation_result<T>(
    result: Result<T, ModerationError>,
    action: &'static str,
) -> Result<T, ApiError> {
    result.map_err(|err| match err {
        ModerationError::NotFound => ApiError::TargetNotFound,
        ModerationError::Backend(msg) => {
            tracing::warn!(action, error = %msg, "livekit moderation failed");
            ApiError::ModerationFailure
        }
    })
}

async fn broadcast_rooms_changed(state: &AppState) {
    let Ok(payload) = serde_json::to_vec(&RoomsChanged {}).map_err(map_signal_err) else {
        return;
    };
    let active_rooms = match state.moderator.list_active_rooms().await {
        Ok(rooms) => rooms,
        Err(ModerationError::NotFound) => return,
        Err(ModerationError::Backend(msg)) => {
            tracing::warn!(error = %msg, "listing active rooms for rooms.changed failed");
            return;
        }
    };
    for room in active_rooms {
        match state
            .moderator
            .send_data(&room, ROOMS_CHANGED_TOPIC, payload.clone(), Vec::new())
            .await
        {
            Ok(()) | Err(ModerationError::NotFound) => {}
            Err(ModerationError::Backend(msg)) => {
                tracing::warn!(%room, error = %msg, "rooms.changed broadcast failed");
            }
        }
    }
}

mod webhooks;

pub(super) use self::webhooks::{admin_create_webhook, admin_delete_webhook, admin_list_webhooks};

#[cfg(test)]
mod tests;
