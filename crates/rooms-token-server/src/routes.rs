use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts, Path, RawQuery, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use livekit_api::access_token::{Claims, TokenVerifier};
use rooms_protocol::{
    ADMIN_CHAT_DELETE_TOPIC, ADMIN_MOVE_TOPIC, ADMIN_STOP_SHARE_TOPIC, AdminChatDelete,
    AdminLoginRequest, AdminLoginResponse, AdminMove, AdminRoom, AdminStopShare,
    ChatHistoryResponse, ChatMessage, CreateRoomRequest, ErrorResponse, MAX_CHAT_ID_LEN,
    MoveParticipantRequest, ROOMS_CHANGED_TOPIC, RoomSummary, RoomsChanged, RoomsResponse,
    SaveChatRequest, TokenRequest, TokenResponse, UpdateInviteRequest, UpdateRoomRequest,
    validate_chat_text, validate_client_id, validate_display_name, validate_invite_code,
    validate_room_name, validate_room_title,
};
use subtle::ConstantTimeEq;

use crate::chat_store::{
    ChatStore, CreateRoomOutcome, DeleteOutcome, DeleteRoomCheck, DeleteRoomOutcome, InsertOutcome,
};
use crate::config::Config;
use crate::directory::{ModerationError, RoomDirectory, RoomModerator};
use crate::token;

const INVITE_HEADER: &str = "x-invite-code";
const CHAT_RATE_LIMIT: usize = 10;
const CHAT_RATE_WINDOW: Duration = Duration::from_secs(10);
const ADMIN_LOGIN_RATE_LIMIT: usize = 5;
const ADMIN_LOGIN_WINDOW: Duration = Duration::from_secs(60);
const RATE_LIMIT_PRUNE_THRESHOLD: usize = 1024;
pub const DEFAULT_ADMIN_FAILURE_DELAY: Duration = Duration::from_millis(250);

struct SlidingWindowLimiter {
    limit: usize,
    window: Duration,
    entries: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl SlidingWindowLimiter {
    fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock_entries(&self) -> MutexGuard<'_, HashMap<String, VecDeque<Instant>>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn prune_map(&self, map: &mut HashMap<String, VecDeque<Instant>>, now: Instant) {
        if map.len() >= RATE_LIMIT_PRUNE_THRESHOLD {
            map.retain(|_, times| {
                prune_expired(times, now, self.window);
                !times.is_empty()
            });
        }
    }

    fn check_and_record_at(&self, key: &str, now: Instant) -> bool {
        let mut map = self.lock_entries();
        self.prune_map(&mut map, now);
        let times = map.entry(key.to_owned()).or_default();
        prune_expired(times, now, self.window);
        if times.len() >= self.limit {
            return false;
        }
        times.push_back(now);
        true
    }

    fn is_allowed_at(&self, keys: &[String], now: Instant) -> bool {
        let mut map = self.lock_entries();
        self.prune_map(&mut map, now);
        for key in keys {
            if let Some(times) = map.get_mut(key) {
                prune_expired(times, now, self.window);
                if times.len() >= self.limit {
                    return false;
                }
            }
        }
        true
    }

    fn record_at(&self, keys: &[String], now: Instant) {
        let mut map = self.lock_entries();
        self.prune_map(&mut map, now);
        for key in keys {
            let times = map.entry(key.clone()).or_default();
            prune_expired(times, now, self.window);
            times.push_back(now);
        }
    }

    fn clear(&self, key: &str) {
        let mut map = self.lock_entries();
        map.remove(key);
    }
}

pub struct ChatRateLimiter(SlidingWindowLimiter);

impl Default for ChatRateLimiter {
    fn default() -> Self {
        Self(SlidingWindowLimiter::new(CHAT_RATE_LIMIT, CHAT_RATE_WINDOW))
    }
}

impl ChatRateLimiter {
    pub fn check(&self, client_id: &str) -> bool {
        self.check_at(client_id, Instant::now())
    }

    fn check_at(&self, client_id: &str, now: Instant) -> bool {
        self.0.check_and_record_at(client_id, now)
    }
}

pub struct AdminLoginLimiter(SlidingWindowLimiter);

impl Default for AdminLoginLimiter {
    fn default() -> Self {
        Self(SlidingWindowLimiter::new(
            ADMIN_LOGIN_RATE_LIMIT,
            ADMIN_LOGIN_WINDOW,
        ))
    }
}

impl AdminLoginLimiter {
    fn is_allowed(&self, keys: &[String]) -> bool {
        self.is_allowed_at(keys, Instant::now())
    }

    fn is_allowed_at(&self, keys: &[String], now: Instant) -> bool {
        self.0.is_allowed_at(keys, now)
    }

    fn record(&self, keys: &[String]) {
        self.record_at(keys, Instant::now());
    }

    fn record_at(&self, keys: &[String], now: Instant) {
        self.0.record_at(keys, now);
    }

    fn clear(&self, key: &str) {
        self.0.clear(key);
    }
}

fn prune_expired(times: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while let Some(&front) = times.front() {
        if now.saturating_duration_since(front) >= window {
            times.pop_front();
        } else {
            break;
        }
    }
}

struct OptionalPeerIp(Option<IpAddr>);

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

fn resolved_client_ip(peer_ip: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
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

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub directory: Arc<dyn RoomDirectory>,
    pub moderator: Arc<dyn RoomModerator>,
    pub chat: Option<Arc<ChatStore>>,
    pub chat_limiter: Arc<ChatRateLimiter>,
    pub admin_login_limiter: Arc<AdminLoginLimiter>,
    pub admin_failure_delay: Duration,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/token", post(issue_token))
        .route("/rooms", get(list_rooms))
        .route("/rooms/{room}/chat", get(list_chat).post(post_chat))
        .route("/rooms/{room}/chat/{id}", delete(delete_chat))
        .route("/admin/login", post(admin_login))
        .route("/admin/rooms", post(admin_create_room))
        .route(
            "/admin/rooms/{id}",
            patch(admin_update_room).delete(admin_delete_room),
        )
        .route(
            "/admin/rooms/{room}/participants/{identity}/mute",
            post(admin_mute_participant),
        )
        .route(
            "/admin/rooms/{room}/participants/{identity}/stop-share",
            post(admin_stop_share_participant),
        )
        .route(
            "/admin/rooms/{room}/participants/{identity}/kick",
            post(admin_kick_participant),
        )
        .route(
            "/admin/rooms/{room}/participants/{identity}/move",
            post(admin_move_participant),
        )
        .route("/admin/rooms/{room}/chat/{id}", delete(admin_delete_chat))
        .route("/admin/invite", post(admin_rotate_invite))
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
    #[error("message id already exists")]
    MessageConflict,
    #[error("maximum room limit reached")]
    RoomLimitReached,
    #[error("cannot delete the last room")]
    CannotDeleteLastRoom,
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("{0}")]
    Invalid(&'static str),
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
            Self::MalformedBody | Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized | Self::InvalidAdminPassword | Self::InvalidToken => {
                StatusCode::UNAUTHORIZED
            }
            Self::ForbiddenAdmin | Self::ForbiddenRoom | Self::ForbiddenMessage => {
                StatusCode::FORBIDDEN
            }
            Self::AdminNotConfigured
            | Self::RoomNotFound
            | Self::TargetNotFound
            | Self::MessageNotFound => StatusCode::NOT_FOUND,
            Self::MessageConflict | Self::RoomLimitReached | Self::CannotDeleteLastRoom => {
                StatusCode::CONFLICT
            }
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

async fn issue_token(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<TokenResponse>, ApiError> {
    let request: TokenRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;

    if !verify_invite(&state, &request.invite_code).await? {
        return Err(ApiError::Unauthorized);
    }
    validate_client_id(&request.client_id).map_err(|_| ApiError::Invalid("invalid client id"))?;
    validate_display_name(&request.display_name)
        .map_err(|_| ApiError::Invalid("invalid display name"))?;
    validate_room_name(&request.room).map_err(|_| ApiError::Invalid("invalid room name"))?;
    if !room_is_listed(&state, &request.room).await? {
        return Err(ApiError::RoomNotFound);
    }

    let name = request.display_name.trim();
    let token =
        token::mint(&state.config, &request.client_id, name, &request.room).map_err(|err| {
            tracing::error!(%err, "minting token failed");
            ApiError::TokenFailure
        })?;

    Ok(Json(TokenResponse {
        token,
        livekit_url: state.config.livekit_url.clone(),
    }))
}

async fn list_rooms(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<RoomsResponse>, ApiError> {
    let provided = headers
        .get(INVITE_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !verify_invite(&state, provided).await? {
        return Err(ApiError::Unauthorized);
    }

    let configured_rooms: Vec<(String, String)> = match &state.chat {
        Some(chat) => chat
            .list_rooms()
            .await
            .map_err(|err| {
                tracing::error!(%err, "room list lookup failed");
                ApiError::ChatStoreFailure
            })?
            .into_iter()
            .map(|r| (r.id, r.title))
            .collect(),
        None => state
            .config
            .rooms
            .iter()
            .map(|name| (name.clone(), name.clone()))
            .collect(),
    };

    let mut rooms = Vec::with_capacity(configured_rooms.len());
    for (name, title) in configured_rooms {
        let participants = state.directory.participants(&name).await.map_err(|err| {
            tracing::warn!(room = %name, %err, "room lookup failed");
            ApiError::DirectoryFailure
        })?;
        rooms.push(RoomSummary {
            name,
            title,
            participants,
        });
    }
    Ok(Json(RoomsResponse { rooms }))
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
    if !token::is_admin_claims(&claims) {
        return Err(ApiError::ForbiddenAdmin);
    }
    Ok(claims.sub)
}

fn validate_chat_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() || id.len() > MAX_CHAT_ID_LEN {
        return Err(ApiError::Invalid("invalid message id"));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    if !id.chars().all(allowed) {
        return Err(ApiError::Invalid("invalid message id"));
    }
    Ok(())
}

fn validate_room_and_identity(room: &str, identity: &str) -> Result<(), ApiError> {
    validate_room_name(room).map_err(|_| ApiError::Invalid("invalid room name"))?;
    validate_client_id(identity).map_err(|_| ApiError::Invalid("invalid participant identity"))?;
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

async fn post_chat(
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

async fn list_chat(
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

async fn delete_chat(
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

async fn admin_login(
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
        token::mint_admin(&state.config, &request.client_id).map_err(|err| {
            tracing::error!(%err, "minting admin token failed");
            ApiError::TokenFailure
        })?;

    tracing::info!(client_id = %request.client_id, "admin logged in");
    Ok(Json(AdminLoginResponse { token, expires_at }))
}

async fn admin_create_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<AdminRoom>), ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    let request: CreateRoomRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    validate_room_title(&request.title).map_err(|_| ApiError::Invalid("invalid room title"))?;

    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    let outcome = chat
        .create_room(request.title.trim().to_owned())
        .await
        .map_err(|err| {
            tracing::error!(%err, "room create failed");
            ApiError::ChatStoreFailure
        })?;

    let room = match outcome {
        CreateRoomOutcome::Created(room) => room,
        CreateRoomOutcome::LimitReached => return Err(ApiError::RoomLimitReached),
    };

    broadcast_rooms_changed(&state).await;
    tracing::info!(%admin_id, room = %room.id, "admin created room");
    Ok((StatusCode::CREATED, Json(room)))
}

async fn admin_update_room(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AdminRoom>, ApiError> {
    let admin_id = authenticate_admin(&state.config, &headers)?;
    validate_room_name(&id).map_err(|_| ApiError::Invalid("invalid room id"))?;
    let request: UpdateRoomRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    if request.title.is_none() && request.position.is_none() {
        return Err(ApiError::Invalid("no fields to update"));
    }
    if let Some(title) = &request.title {
        validate_room_title(title).map_err(|_| ApiError::Invalid("invalid room title"))?;
    }

    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    let room = chat
        .update_room(
            id,
            request.title.map(|t| t.trim().to_owned()),
            request.position,
        )
        .await
        .map_err(|err| {
            tracing::error!(%err, "room update failed");
            ApiError::ChatStoreFailure
        })?
        .ok_or(ApiError::RoomNotFound)?;

    broadcast_rooms_changed(&state).await;
    tracing::info!(%admin_id, room = %room.id, "admin updated room");
    Ok(Json(room))
}

async fn admin_delete_room(
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

async fn admin_mute_participant(
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

async fn admin_stop_share_participant(
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

async fn admin_kick_participant(
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

async fn admin_move_participant(
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

    if !room_is_listed(&state, &request.to_room).await? {
        return Err(ApiError::RoomNotFound);
    }

    map_moderation_result(
        state.moderator.ensure_participant(&room, &identity).await,
        "ensure_participant",
    )?;

    let payload = serde_json::to_vec(&AdminMove {
        to_room: request.to_room.clone(),
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

async fn admin_delete_chat(
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

async fn admin_rotate_invite(
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

async fn room_is_listed(state: &AppState, room: &str) -> Result<bool, ApiError> {
    match &state.chat {
        Some(chat) => chat.room_exists(room.to_owned()).await.map_err(|err| {
            tracing::error!(%err, "room lookup failed");
            ApiError::ChatStoreFailure
        }),
        None => Ok(state.config.rooms.iter().any(|r| r == room)),
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
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use livekit_api::access_token::{AccessToken, TokenVerifier, VideoGrants};
    use rooms_protocol::{
        MAX_CHAT_MESSAGE_LEN, MAX_ROOMS_COUNT, SavedChatMessage, validate_room_name,
    };
    use tower::ServiceExt;

    use super::*;
    use crate::directory::{
        FakeDirectory, ModerationCall, TRACK_SOURCE_MICROPHONE, TRACK_SOURCE_SCREEN_SHARE,
        TRACK_SOURCE_SCREEN_SHARE_AUDIO,
    };

    const INVITE: &str = "s3cret-invite";
    const ADMIN_PASSWORD: &str = "admin-pass-1234";
    const API_KEY: &str = "test-key";
    const API_SECRET: &str = "test-secret-with-enough-length-0123456789";
    const TTL: u64 = 600;

    fn cached_admin_hash() -> String {
        static HASH: OnceLock<String> = OnceLock::new();
        HASH.get_or_init(|| crate::config::hash_secret(ADMIN_PASSWORD).unwrap())
            .clone()
    }

    fn test_config() -> Config {
        Config {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            invite_code: INVITE.to_owned(),
            invite_code_hash: None,
            admin_code_hash: None,
            livekit_url: "ws://livekit.test:7880".to_owned(),
            livekit_api_key: API_KEY.to_owned(),
            livekit_api_secret: API_SECRET.to_owned(),
            token_ttl_secs: TTL,
            rooms: vec!["geral".to_owned(), "jogos".to_owned(), "afk".to_owned()],
            chat_db_path: PathBuf::from("chat.db"),
        }
    }

    fn test_admin_config() -> Config {
        let mut config = test_config();
        config.admin_code_hash = Some(cached_admin_hash());
        config
    }

    fn default_fake_dir() -> Arc<FakeDirectory> {
        Arc::new(
            FakeDirectory::default()
                .with_participants("geral", &["ana", "bia"])
                .with_participants("afk", &["caio"]),
        )
    }

    fn seeded_store(rooms: &[String]) -> Arc<ChatStore> {
        let store = ChatStore::open_in_memory().unwrap();
        store.seed_rooms_if_empty_sync(rooms).unwrap();
        Arc::new(store)
    }

    fn state_with(
        config: Config,
        directory: Arc<FakeDirectory>,
        chat: Option<Arc<ChatStore>>,
        admin_failure_delay: Duration,
    ) -> AppState {
        AppState {
            config: Arc::new(config),
            directory: directory.clone(),
            moderator: directory,
            chat,
            chat_limiter: Arc::new(ChatRateLimiter::default()),
            admin_login_limiter: Arc::new(AdminLoginLimiter::default()),
            admin_failure_delay,
        }
    }

    fn app_with(directory: FakeDirectory) -> Router {
        let config = test_config();
        let chat = Some(seeded_store(&config.rooms));
        router(state_with(
            config,
            Arc::new(directory),
            chat,
            Duration::ZERO,
        ))
    }

    fn app_with_config(config: Config) -> Router {
        let chat = Some(seeded_store(&config.rooms));
        router(state_with(config, default_fake_dir(), chat, Duration::ZERO))
    }

    fn app_with_store(chat: Option<Arc<ChatStore>>) -> Router {
        router(state_with(
            test_config(),
            default_fake_dir(),
            chat,
            Duration::ZERO,
        ))
    }

    fn app() -> Router {
        let config = test_config();
        let chat = Some(seeded_store(&config.rooms));
        router(state_with(config, default_fake_dir(), chat, Duration::ZERO))
    }

    fn token_request(body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/token")
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    fn token_body_with_id(client_id: &str, display_name: &str, room: &str, invite: &str) -> String {
        serde_json::to_string(&TokenRequest {
            client_id: client_id.to_owned(),
            display_name: display_name.to_owned(),
            room: room.to_owned(),
            invite_code: invite.to_owned(),
        })
        .unwrap()
    }

    fn token_body(display_name: &str, room: &str, invite: &str) -> String {
        token_body_with_id("client-pedro", display_name, room, invite)
    }

    fn rooms_request(invite: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/rooms");
        if let Some(invite) = invite {
            builder = builder.header(INVITE_HEADER, invite);
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn body_bytes(response: Response) -> Vec<u8> {
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    async fn error_message(response: Response) -> String {
        let parsed: ErrorResponse = serde_json::from_slice(&body_bytes(response).await).unwrap();
        parsed.error
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

    #[tokio::test]
    async fn token_rejects_wrong_invite() {
        let body = token_body("Pedro", "geral", "wrong");
        let response = app().oneshot(token_request(&body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let message = error_message(response).await;
        assert!(!message.contains(INVITE));
    }

    #[tokio::test]
    async fn token_rejects_bad_room_name() {
        let body = token_body("Pedro", "Sala Ruim", INVITE);
        let response = app().oneshot(token_request(&body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn token_rejects_unlisted_room() {
        let body = token_body("Pedro", "apagada", INVITE);
        let response = app().oneshot(token_request(&body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let config = test_config();
        let no_db = router(state_with(config, default_fake_dir(), None, Duration::ZERO));
        let response = no_db.oneshot(token_request(&body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn token_rejects_empty_display_name() {
        let body = token_body("   ", "geral", INVITE);
        let response = app().oneshot(token_request(&body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn token_rejects_malformed_json() {
        for body in ["{not json", "", r#"{"display_name":"a"}"#] {
            let response = app().oneshot(token_request(body)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body:?}");
        }
    }

    #[tokio::test]
    async fn token_checks_invite_before_field_validation() {
        let body = token_body("", "Bad Room", "wrong");
        let response = app().oneshot(token_request(&body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_rejects_bad_client_id() {
        for bad in ["", "Pedro", "a_b", "a b", &"a".repeat(65)] {
            let body = token_body_with_id(bad, "Pedro", "geral", INVITE);
            let response = app().oneshot(token_request(&body)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn token_success_carries_room_identity_and_expiry() {
        let body = token_body_with_id("inst-0123abcdef", "  Pedro  ", "jogos", INVITE);
        let response = app().oneshot(token_request(&body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let parsed: TokenResponse = serde_json::from_slice(&body_bytes(response).await).unwrap();
        assert_eq!(parsed.livekit_url, "ws://livekit.test:7880");

        let claims = TokenVerifier::with_api_key(API_KEY, API_SECRET)
            .verify(&parsed.token)
            .unwrap();
        assert_eq!(claims.sub, "inst-0123abcdef");
        assert_eq!(claims.name, "Pedro");
        assert_eq!(claims.iss, API_KEY);
        assert!(claims.video.room_join);
        assert_eq!(claims.video.room, "jogos");
        assert!(claims.video.can_publish());
        assert!(claims.video.can_subscribe());
        assert!(claims.video.can_publish_data());
        assert!(!claims.video.room_admin);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize;
        let ttl = TTL as usize;
        assert!(claims.exp >= now + ttl - 5 && claims.exp <= now + ttl + 5);
    }

    #[tokio::test]
    async fn hashed_invite_works_for_token_and_rooms() {
        let hash = crate::config::hash_password(&format!("{INVITE}\n")).unwrap();
        let mut config = test_config();
        config.invite_code = String::new();
        config.invite_code_hash = Some(hash);

        let ok_body = token_body("Pedro", "geral", INVITE);
        let response = app_with_config(config.clone())
            .oneshot(token_request(&ok_body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bad_body = token_body("Pedro", "geral", "wrong");
        let response = app_with_config(config.clone())
            .oneshot(token_request(&bad_body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app_with_config(config.clone())
            .oneshot(rooms_request(Some(INVITE)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        for bad in [None, Some("wrong"), Some("")] {
            let response = app_with_config(config.clone())
                .oneshot(rooms_request(bad))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn rooms_require_invite_header() {
        for invite in [None, Some("wrong"), Some("")] {
            let response = app().oneshot(rooms_request(invite)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{invite:?}");
        }
    }

    #[tokio::test]
    async fn rooms_list_configured_rooms_in_order() {
        let response = app().oneshot(rooms_request(Some(INVITE))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let parsed: RoomsResponse = serde_json::from_slice(&body_bytes(response).await).unwrap();
        let summary = |name: &str, people: &[&str]| RoomSummary {
            name: name.to_owned(),
            title: name.to_owned(),
            participants: people.iter().map(|p| (*p).to_owned()).collect(),
        };
        assert_eq!(
            parsed.rooms,
            vec![
                summary("geral", &["ana", "bia"]),
                summary("jogos", &[]),
                summary("afk", &["caio"]),
            ]
        );
    }

    #[tokio::test]
    async fn rooms_return_bad_gateway_when_directory_fails() {
        let app = app_with(FakeDirectory::default().failing_on("jogos"));
        let response = app.oneshot(rooms_request(Some(INVITE))).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let message = error_message(response).await;
        assert!(!message.contains("fake failure"));
    }

    #[test]
    fn invite_comparison() {
        assert!(invite_matches("abc", "abc"));
        assert!(!invite_matches("abc", "abd"));
        assert!(!invite_matches("abc", "ab"));
        assert!(!invite_matches("abc", "abc\0"));
        assert!(!invite_matches("abc", ""));
    }

    fn mint_room_jwt(client_id: &str, name: &str, room: &str, room_join: bool) -> String {
        AccessToken::with_api_key(API_KEY, API_SECRET)
            .with_ttl(Duration::from_secs(TTL))
            .with_identity(client_id)
            .with_name(name)
            .with_grants(VideoGrants {
                room_join,
                room: room.to_owned(),
                can_publish: Some(true),
                can_subscribe: Some(true),
                can_publish_data: Some(true),
                ..Default::default()
            })
            .to_jwt()
            .unwrap()
    }

    fn base64url_encode(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
            let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
            if chunk.len() > 1 {
                out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(TABLE[(n & 0x3f) as usize] as char);
            }
        }
        out
    }

    fn sha256(data: &[u8]) -> [u8; 32] {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let bit_len = (data.len() as u64) * 8;
        let mut padded = data.to_vec();
        padded.push(0x80);
        while (padded.len() % 64) != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&bit_len.to_be_bytes());
        for block in padded.chunks_exact(64) {
            let mut w = [0_u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    block[4 * i],
                    block[4 * i + 1],
                    block[4 * i + 2],
                    block[4 * i + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let temp1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let temp2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(temp1);
                d = c;
                c = b;
                b = a;
                a = temp1.wrapping_add(temp2);
            }
            for (slot, val) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *slot = slot.wrapping_add(val);
            }
        }
        let mut out = [0_u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[4 * i..4 * (i + 1)].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn sign_hs256_jwt(claims_json: &str, secret: &str) -> String {
        let header = base64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = base64url_encode(claims_json.as_bytes());
        let signing_input = format!("{header}.{payload}");
        let mut key = [0_u8; 64];
        let secret_bytes = secret.as_bytes();
        if secret_bytes.len() > 64 {
            let digest = sha256(secret_bytes);
            key[..32].copy_from_slice(&digest);
        } else {
            key[..secret_bytes.len()].copy_from_slice(secret_bytes);
        }
        let mut ipad = [0x36_u8; 64];
        let mut opad = [0x5c_u8; 64];
        for i in 0..64 {
            ipad[i] ^= key[i];
            opad[i] ^= key[i];
        }
        let mut inner = ipad.to_vec();
        inner.extend_from_slice(signing_input.as_bytes());
        let inner_hash = sha256(&inner);
        let mut outer = opad.to_vec();
        outer.extend_from_slice(&inner_hash);
        let sig = base64url_encode(&sha256(&outer));
        format!("{signing_input}.{sig}")
    }

    fn post_chat_req(room: &str, auth: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/rooms/{room}/chat"))
            .header("content-type", "application/json");
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    fn get_chat_req(uri: &str, auth: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn delete_chat_req(room: &str, id: &str, auth: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("DELETE")
            .uri(format!("/rooms/{room}/chat/{id}"));
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn save_chat_body(id: &str, text: &str, sent_at_unix_ms: u64) -> String {
        serde_json::to_string(&SaveChatRequest {
            id: id.to_owned(),
            text: text.to_owned(),
            sent_at_unix_ms,
        })
        .unwrap()
    }

    #[test]
    fn rate_limiter_allows_10_per_10s_and_resets_after_window() {
        let limiter = ChatRateLimiter::default();
        let t0 = Instant::now();
        for _ in 0..10 {
            assert!(limiter.check_at("client-a", t0));
        }
        assert!(!limiter.check_at("client-a", t0 + Duration::from_secs(9)));
        assert!(limiter.check_at("client-b", t0 + Duration::from_secs(9)));
        assert!(limiter.check_at("client-a", t0 + Duration::from_secs(10)));
    }

    #[tokio::test]
    async fn chat_routes_require_valid_bearer_token() {
        let router = app();
        let expired = sign_hs256_jwt(
            r#"{"exp":1,"iss":"test-key","nbf":0,"sub":"client-a","name":"Ana","video":{"roomJoin":true,"room":"geral"}}"#,
            API_SECRET,
        );
        let empty_sub = sign_hs256_jwt(
            &format!(
                r#"{{"exp":{},"iss":"test-key","nbf":0,"sub":"","name":"Ana","video":{{"roomJoin":true,"room":"geral"}}}}"#,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    + 600
            ),
            API_SECRET,
        );
        let wrong_secret = AccessToken::with_api_key(API_KEY, "wrong-secret-01234567890123456789")
            .with_ttl(Duration::from_secs(600))
            .with_identity("client-a")
            .with_name("Ana")
            .with_grants(VideoGrants {
                room_join: true,
                room: "geral".to_owned(),
                ..Default::default()
            })
            .to_jwt()
            .unwrap();

        let bad_headers: Vec<Option<String>> = vec![
            None,
            Some("Basic abc".to_owned()),
            Some("Bearer   ".to_owned()),
            Some("Bearer not.a.jwt".to_owned()),
            Some(format!("Bearer {wrong_secret}")),
            Some(format!("Bearer {expired}")),
            Some(format!("Bearer {empty_sub}")),
        ];

        for auth in bad_headers {
            let post_res = router
                .clone()
                .oneshot(post_chat_req(
                    "geral",
                    auth.as_deref(),
                    &save_chat_body("m1", "oi", 100),
                ))
                .await
                .unwrap();
            assert_eq!(post_res.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
            assert!(!error_message(post_res).await.is_empty());

            let get_res = router
                .clone()
                .oneshot(get_chat_req("/rooms/geral/chat", auth.as_deref()))
                .await
                .unwrap();
            assert_eq!(get_res.status(), StatusCode::UNAUTHORIZED, "{auth:?}");

            let del_res = router
                .clone()
                .oneshot(delete_chat_req("geral", "m1", auth.as_deref()))
                .await
                .unwrap();
            assert_eq!(del_res.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
        }
    }

    #[tokio::test]
    async fn chat_routes_reject_wrong_room_or_missing_room_join() {
        let router = app();
        let geral_jwt = mint_room_jwt("client-a", "Ana", "geral", true);
        let no_join_jwt = mint_room_jwt("client-a", "Ana", "jogos", false);

        for jwt in [geral_jwt, no_join_jwt] {
            let auth = format!("Bearer {jwt}");
            let post_res = router
                .clone()
                .oneshot(post_chat_req(
                    "jogos",
                    Some(&auth),
                    &save_chat_body("m1", "oi", 100),
                ))
                .await
                .unwrap();
            assert_eq!(post_res.status(), StatusCode::FORBIDDEN);

            let get_res = router
                .clone()
                .oneshot(get_chat_req("/rooms/jogos/chat", Some(&auth)))
                .await
                .unwrap();
            assert_eq!(get_res.status(), StatusCode::FORBIDDEN);

            let del_res = router
                .clone()
                .oneshot(delete_chat_req("jogos", "m1", Some(&auth)))
                .await
                .unwrap();
            assert_eq!(del_res.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn chat_routes_reject_invalid_input() {
        let router = app();
        let jwt = mint_room_jwt("client-a", "Ana", "geral", true);
        let auth = format!("Bearer {jwt}");

        let bad_room = router
            .clone()
            .oneshot(get_chat_req("/rooms/Sala%20Ruim/chat", Some(&auth)))
            .await
            .unwrap();
        assert_eq!(bad_room.status(), StatusCode::BAD_REQUEST);

        for raw_body in ["not json", ""] {
            let res = router
                .clone()
                .oneshot(post_chat_req("geral", Some(&auth), raw_body))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        }

        for bad_id in [
            "",
            &"a".repeat(MAX_CHAT_ID_LEN + 1),
            "bad id",
            "bad!id",
            "é",
        ] {
            let res = router
                .clone()
                .oneshot(post_chat_req(
                    "geral",
                    Some(&auth),
                    &save_chat_body(bad_id, "oi", 100),
                ))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad_id:?}");
        }

        for bad_text in ["", "   \n\t ", &"é".repeat(MAX_CHAT_MESSAGE_LEN + 1)] {
            let res = router
                .clone()
                .oneshot(post_chat_req(
                    "geral",
                    Some(&auth),
                    &save_chat_body("m-1", bad_text, 100),
                ))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        }

        for bad_query in [
            "/rooms/geral/chat?before=",
            "/rooms/geral/chat?before=bad%20id",
            "/rooms/geral/chat?before=%ZZ",
        ] {
            let res = router
                .clone()
                .oneshot(get_chat_req(bad_query, Some(&auth)))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad_query}");
        }

        let bad_del_id = router
            .clone()
            .oneshot(delete_chat_req("geral", "bad!id", Some(&auth)))
            .await
            .unwrap();
        assert_eq!(bad_del_id.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn chat_post_enforces_rate_limit_per_client_id() {
        let router = app();
        let ana_auth = format!(
            "Bearer {}",
            mint_room_jwt("client-ana", "Ana", "geral", true)
        );
        let bia_auth = format!(
            "Bearer {}",
            mint_room_jwt("client-bia", "Bia", "geral", true)
        );

        for i in 0..10 {
            let res = router
                .clone()
                .oneshot(post_chat_req(
                    "geral",
                    Some(&ana_auth),
                    &save_chat_body(&format!("m-{i}"), "oi", 1000 + i),
                ))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::CREATED);
        }

        let limited = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&ana_auth),
                &save_chat_body("m-10", "spam", 1010),
            ))
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(!error_message(limited).await.is_empty());

        let bia_res = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&bia_auth),
                &save_chat_body("bia-1", "oi", 1011),
            ))
            .await
            .unwrap();
        assert_eq!(bia_res.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn chat_delete_enforces_ownership_and_missing() {
        let router = app();
        let ana_auth = format!(
            "Bearer {}",
            mint_room_jwt("client-ana", "Ana", "geral", true)
        );
        let bia_auth = format!(
            "Bearer {}",
            mint_room_jwt("client-bia", "Bia", "geral", true)
        );

        let created = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&ana_auth),
                &save_chat_body("m-1", "minha mensagem", 1000),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let forbidden = router
            .clone()
            .oneshot(delete_chat_req("geral", "m-1", Some(&bia_auth)))
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

        let missing = router
            .clone()
            .oneshot(delete_chat_req("geral", "does-not-exist", Some(&ana_auth)))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let deleted = router
            .clone()
            .oneshot(delete_chat_req("geral", "m-1", Some(&ana_auth)))
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert!(body_bytes(deleted).await.is_empty());

        let deleted_again = router
            .clone()
            .oneshot(delete_chat_req("geral", "m-1", Some(&ana_auth)))
            .await
            .unwrap();
        assert_eq!(deleted_again.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn token_to_post_to_get_uses_token_identity_and_ignores_forged_author() {
        let router = app();

        let tok_body = token_body_with_id("client-ana-01", "  Ana Maria  ", "geral", INVITE);
        let tok_res = router
            .clone()
            .oneshot(token_request(&tok_body))
            .await
            .unwrap();
        assert_eq!(tok_res.status(), StatusCode::OK);
        let TokenResponse { token, .. } =
            serde_json::from_slice(&body_bytes(tok_res).await).unwrap();
        let ana_auth = format!("Bearer {token}");

        let now_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let far_future = now_ms + 3_600_000;
        let forged_body = format!(
            r#"{{"id":"msg_1","text":"salva aqui","sent_at_unix_ms":{far_future},"sender":"Admin","sender_id":"forged-id","saved":false}}"#
        );

        let post_res = router
            .clone()
            .oneshot(post_chat_req("geral", Some(&ana_auth), &forged_body))
            .await
            .unwrap();
        assert_eq!(post_res.status(), StatusCode::CREATED);
        let posted: ChatMessage = serde_json::from_slice(&body_bytes(post_res).await).unwrap();
        assert_eq!(posted.id, "msg_1");
        assert_eq!(posted.sender, "Ana Maria");
        assert_eq!(posted.text, "salva aqui");
        assert!(posted.saved);
        assert!(posted.sent_at_unix_ms <= now_ms + 65_000);

        let dup_res = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&ana_auth),
                &save_chat_body("msg_1", "tentativa 2", now_ms),
            ))
            .await
            .unwrap();
        assert_eq!(dup_res.status(), StatusCode::OK);
        let dup_msg: ChatMessage = serde_json::from_slice(&body_bytes(dup_res).await).unwrap();
        assert_eq!(dup_msg, posted);

        let bia_auth = format!(
            "Bearer {}",
            mint_room_jwt("client-bia", "Bia", "geral", true)
        );
        let conflict_res = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&bia_auth),
                &save_chat_body("msg_1", "sequestro", now_ms),
            ))
            .await
            .unwrap();
        assert_eq!(conflict_res.status(), StatusCode::CONFLICT);

        let get_res = router
            .clone()
            .oneshot(get_chat_req("/rooms/geral/chat", Some(&bia_auth)))
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::OK);
        let history: ChatHistoryResponse =
            serde_json::from_slice(&body_bytes(get_res).await).unwrap();
        assert_eq!(
            history,
            ChatHistoryResponse {
                messages: vec![SavedChatMessage {
                    message: posted,
                    sender_id: "client-ana-01".to_owned(),
                }],
                has_more: false,
            }
        );
    }

    #[tokio::test]
    async fn get_chat_paginates_chronologically_with_before() {
        let store = Arc::new(ChatStore::open_in_memory().unwrap());
        for i in 0..105_u64 {
            store
                .insert_at(
                    "geral".to_owned(),
                    format!("m-{i:03}"),
                    "client-a".to_owned(),
                    "Ana".to_owned(),
                    format!("msg {i}"),
                    1_000 + i,
                    2_000 + i,
                )
                .await
                .unwrap();
        }
        let router = app_with_store(Some(store));
        let auth = format!("Bearer {}", mint_room_jwt("client-a", "Ana", "geral", true));

        let page1_res = router
            .clone()
            .oneshot(get_chat_req("/rooms/geral/chat", Some(&auth)))
            .await
            .unwrap();
        assert_eq!(page1_res.status(), StatusCode::OK);
        let page1: ChatHistoryResponse =
            serde_json::from_slice(&body_bytes(page1_res).await).unwrap();
        assert!(page1.has_more);
        assert_eq!(page1.messages.len(), 100);
        assert_eq!(page1.messages[0].message.id, "m-005");
        assert_eq!(page1.messages[99].message.id, "m-104");

        let page2_res = router
            .clone()
            .oneshot(get_chat_req("/rooms/geral/chat?before=m-005", Some(&auth)))
            .await
            .unwrap();
        assert_eq!(page2_res.status(), StatusCode::OK);
        let page2: ChatHistoryResponse =
            serde_json::from_slice(&body_bytes(page2_res).await).unwrap();
        assert!(!page2.has_more);
        let ids: Vec<&str> = page2
            .messages
            .iter()
            .map(|m| m.message.id.as_str())
            .collect();
        assert_eq!(ids, vec!["m-000", "m-001", "m-002", "m-003", "m-004"]);
    }

    #[tokio::test]
    async fn chat_returns_503_when_db_unavailable_while_token_works() {
        let missing_dir = PathBuf::from("/nonexistent-zenha-dir-for-test/sub/chat.db");
        assert!(ChatStore::open(&missing_dir).is_err());

        let router = app_with_store(None);
        let tok_body = token_body("Ana", "geral", INVITE);
        let tok_res = router
            .clone()
            .oneshot(token_request(&tok_body))
            .await
            .unwrap();
        assert_eq!(tok_res.status(), StatusCode::OK);
        let TokenResponse { token, .. } =
            serde_json::from_slice(&body_bytes(tok_res).await).unwrap();
        let auth = format!("Bearer {token}");

        let post_res = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&auth),
                &save_chat_body("m-1", "oi", 1000),
            ))
            .await
            .unwrap();
        assert_eq!(post_res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!error_message(post_res).await.is_empty());

        let get_res = router
            .clone()
            .oneshot(get_chat_req("/rooms/geral/chat", Some(&auth)))
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::SERVICE_UNAVAILABLE);

        let del_res = router
            .clone()
            .oneshot(delete_chat_req("geral", "m-1", Some(&auth)))
            .await
            .unwrap();
        assert_eq!(del_res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    fn json_request(method: &str, uri: &str, auth: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    fn admin_app_with(
        dir: Arc<FakeDirectory>,
        chat: Option<Arc<ChatStore>>,
        delay: Duration,
    ) -> Router {
        router(state_with(test_admin_config(), dir, chat, delay))
    }

    async fn login_admin(app: &Router) -> String {
        let body = serde_json::to_string(&AdminLoginRequest {
            password: ADMIN_PASSWORD.to_owned(),
            client_id: "admin-client".to_owned(),
        })
        .unwrap();
        let res = app
            .clone()
            .oneshot(json_request("POST", "/admin/login", None, &body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let parsed: AdminLoginResponse = serde_json::from_slice(&body_bytes(res).await).unwrap();
        format!("Bearer {}", parsed.token)
    }

    #[tokio::test]
    async fn admin_routes_return_404_when_admin_code_unset() {
        let app = app();
        let room_jwt = format!(
            "Bearer {}",
            mint_room_jwt("client-pedro", "Pedro", "geral", true)
        );
        let cases = [
            json_request(
                "POST",
                "/admin/login",
                None,
                r#"{"password":"x","client_id":"a"}"#,
            ),
            json_request(
                "POST",
                "/admin/rooms",
                Some(&room_jwt),
                r#"{"title":"Nova"}"#,
            ),
            json_request(
                "PATCH",
                "/admin/rooms/geral",
                Some(&room_jwt),
                r#"{"title":"G"}"#,
            ),
            json_request("DELETE", "/admin/rooms/geral", Some(&room_jwt), ""),
            json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/mute",
                Some(&room_jwt),
                "",
            ),
            json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/stop-share",
                Some(&room_jwt),
                "",
            ),
            json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/kick",
                Some(&room_jwt),
                "",
            ),
            json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/move",
                Some(&room_jwt),
                r#"{"to_room":"afk"}"#,
            ),
            json_request("DELETE", "/admin/rooms/geral/chat/m-1", Some(&room_jwt), ""),
            json_request(
                "POST",
                "/admin/invite",
                Some(&room_jwt),
                r#"{"new_code":"new-invite-code"}"#,
            ),
        ];
        for req in cases {
            let uri = req.uri().to_string();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn admin_login_succeeds_and_issues_admin_only_jwt() {
        let config = test_admin_config();
        let app = admin_app_with(
            default_fake_dir(),
            Some(seeded_store(&config.rooms)),
            Duration::ZERO,
        );
        let body = serde_json::to_string(&AdminLoginRequest {
            password: ADMIN_PASSWORD.to_owned(),
            client_id: "admin-1".to_owned(),
        })
        .unwrap();
        let res = app
            .oneshot(json_request("POST", "/admin/login", None, &body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let parsed: AdminLoginResponse = serde_json::from_slice(&body_bytes(res).await).unwrap();

        let claims = TokenVerifier::with_api_key(API_KEY, API_SECRET)
            .verify(&parsed.token)
            .unwrap();
        assert!(token::is_admin_claims(&claims));
        assert_eq!(claims.sub, "admin-1");
        assert!(!claims.video.room_join);
        assert!(!claims.video.room_admin);
        assert!(!claims.video.room_create);
        assert!(!claims.video.room_list);
        assert!(claims.video.room.is_empty());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(parsed.expires_at >= now + TTL - 5 && parsed.expires_at <= now + TTL + 5);
    }

    #[tokio::test]
    async fn admin_login_wrong_password_applies_delay_and_hides_secrets() {
        let config = test_admin_config();
        let app = admin_app_with(
            default_fake_dir(),
            Some(seeded_store(&config.rooms)),
            Duration::from_millis(80),
        );
        let body = serde_json::to_string(&AdminLoginRequest {
            password: "wrong-password-123".to_owned(),
            client_id: "admin-1".to_owned(),
        })
        .unwrap();

        let start = Instant::now();
        let res = app
            .oneshot(json_request("POST", "/admin/login", None, &body))
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(elapsed >= Duration::from_millis(60), "elapsed={elapsed:?}");
        let msg = error_message(res).await;
        assert!(!msg.contains("wrong-password-123"));
        assert!(!msg.contains(ADMIN_PASSWORD));
        assert!(!msg.contains("argon2"));
    }

    #[tokio::test]
    async fn admin_login_rate_limits_by_client_id_and_ip() {
        let config = test_admin_config();
        let app = admin_app_with(
            default_fake_dir(),
            Some(seeded_store(&config.rooms)),
            Duration::ZERO,
        );

        let wrong_for = |cid: &str| {
            serde_json::to_string(&AdminLoginRequest {
                password: "wrong".to_owned(),
                client_id: cid.to_owned(),
            })
            .unwrap()
        };

        // Five failures for client-a trigger 429 on the 6th attempt.
        for attempt in 0..5 {
            let res = app
                .clone()
                .oneshot(json_request(
                    "POST",
                    "/admin/login",
                    None,
                    &wrong_for("client-a"),
                ))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "attempt {attempt}");
        }
        let limited = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/login",
                None,
                &wrong_for("client-a"),
            ))
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);

        // Five failures from one public IP with rotating client_ids trigger 429 on the 6th.
        let ip_a: SocketAddr = "203.0.113.10:4000".parse().unwrap();
        for i in 0..5 {
            let mut req = json_request(
                "POST",
                "/admin/login",
                None,
                &wrong_for(&format!("rot-{i}")),
            );
            req.extensions_mut().insert(ConnectInfo(ip_a));
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "ip attempt {i}");
        }
        let mut sixth = json_request("POST", "/admin/login", None, &wrong_for("rot-fresh"));
        sixth.extensions_mut().insert(ConnectInfo(ip_a));
        let limited_ip = app.clone().oneshot(sixth).await.unwrap();
        assert_eq!(limited_ip.status(), StatusCode::TOO_MANY_REQUESTS);

        // Behind loopback Caddy, X-Forwarded-For is honored cleanly.
        let loopback: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        for i in 0..5 {
            let mut req = json_request(
                "POST",
                "/admin/login",
                None,
                &wrong_for(&format!("caddy-{i}")),
            );
            req.extensions_mut().insert(ConnectInfo(loopback));
            req.headers_mut()
                .insert("x-forwarded-for", "1.1.1.1, 198.51.100.42".parse().unwrap());
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }
        let mut caddy_sixth = json_request("POST", "/admin/login", None, &wrong_for("caddy-fresh"));
        caddy_sixth.extensions_mut().insert(ConnectInfo(loopback));
        caddy_sixth
            .headers_mut()
            .insert("x-forwarded-for", "198.51.100.42".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(caddy_sixth).await.unwrap().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn admin_and_room_tokens_are_mutually_isolated() {
        let config = test_admin_config();
        let app = admin_app_with(
            default_fake_dir(),
            Some(seeded_store(&config.rooms)),
            Duration::ZERO,
        );
        let admin_auth = login_admin(&app).await;
        let room_auth = format!(
            "Bearer {}",
            mint_room_jwt("client-pedro", "Pedro", "geral", true)
        );

        // Room JWT rejected with 403 on admin routes.
        for req in [
            json_request(
                "POST",
                "/admin/rooms",
                Some(&room_auth),
                r#"{"title":"Nova"}"#,
            ),
            json_request(
                "PATCH",
                "/admin/rooms/geral",
                Some(&room_auth),
                r#"{"title":"X"}"#,
            ),
            json_request("DELETE", "/admin/rooms/geral", Some(&room_auth), ""),
            json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/mute",
                Some(&room_auth),
                "",
            ),
            json_request(
                "DELETE",
                "/admin/rooms/geral/chat/m-1",
                Some(&room_auth),
                "",
            ),
            json_request(
                "POST",
                "/admin/invite",
                Some(&room_auth),
                r#"{"new_code":"new-invite-code"}"#,
            ),
        ] {
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
        }

        // Missing or bad bearer returns 401 on admin routes.
        let no_auth = json_request("POST", "/admin/rooms", None, r#"{"title":"Nova"}"#);
        assert_eq!(
            app.clone().oneshot(no_auth).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let bad_auth = json_request(
            "POST",
            "/admin/rooms",
            Some("Bearer invalid-jwt"),
            r#"{"title":"Nova"}"#,
        );
        assert_eq!(
            app.clone().oneshot(bad_auth).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        // Admin JWT cannot post/list/delete on room chat routes.
        assert_eq!(
            app.clone()
                .oneshot(post_chat_req(
                    "geral",
                    Some(&admin_auth),
                    &save_chat_body("m-1", "oi", 1000)
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(get_chat_req("/rooms/geral/chat", Some(&admin_auth)))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(delete_chat_req("geral", "m-1", Some(&admin_auth)))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn admin_room_crud_lifecycle_and_limit() {
        let config = test_admin_config();
        let dir = Arc::new(
            FakeDirectory::default()
                .with_participants("geral", &["ana"])
                .with_participants("afk", &["caio"]),
        );
        let store = seeded_store(&config.rooms);
        let app = router(state_with(
            config,
            dir.clone(),
            Some(store.clone()),
            Duration::ZERO,
        ));
        let admin_auth = login_admin(&app).await;

        // Save chat in "afk" and "geral" to verify room deletion clears only "afk".
        let afk_jwt = format!(
            "Bearer {}",
            mint_room_jwt("client-caio", "Caio", "afk", true)
        );
        let geral_jwt = format!(
            "Bearer {}",
            mint_room_jwt("client-ana", "Ana", "geral", true)
        );
        assert_eq!(
            app.clone()
                .oneshot(post_chat_req(
                    "afk",
                    Some(&afk_jwt),
                    &save_chat_body("m-afk", "bye", 100)
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            app.clone()
                .oneshot(post_chat_req(
                    "geral",
                    Some(&geral_jwt),
                    &save_chat_body("m-ger", "hi", 101)
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );

        // Create accented room -> valid slug + unique suffix + broadcasts rooms.changed to active rooms.
        let create_res = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/rooms",
                Some(&admin_auth),
                r#"{"title":"Mesa de Café"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(create_res.status(), StatusCode::CREATED);
        let created: AdminRoom = serde_json::from_slice(&body_bytes(create_res).await).unwrap();
        assert_eq!(created.id, "mesa-de-cafe");
        assert!(validate_room_name(&created.id).is_ok());
        assert_eq!(created.title, "Mesa de Café");

        let dup_res = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/rooms",
                Some(&admin_auth),
                r#"{"title":"Mesa de Café"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(dup_res.status(), StatusCode::CREATED);
        let dup: AdminRoom = serde_json::from_slice(&body_bytes(dup_res).await).unwrap();
        assert_eq!(dup.id, "mesa-de-cafe-2");

        // Verify rooms.changed sent to active rooms ("geral", "afk") only.
        let calls_after_create = dir.calls();
        let changed_rooms: Vec<&str> = calls_after_create
            .iter()
            .filter_map(|c| match c {
                ModerationCall::SendData { room, topic, .. } if topic == ROOMS_CHANGED_TOPIC => {
                    Some(room.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(changed_rooms.contains(&"geral"));
        assert!(changed_rooms.contains(&"afk"));
        assert!(!changed_rooms.contains(&"jogos"));

        // Patch room title & position (preserves ID and updates GET /rooms order).
        let patch_res = app
            .clone()
            .oneshot(json_request(
                "PATCH",
                "/admin/rooms/jogos",
                Some(&admin_auth),
                r#"{"title":"Jogatina","position":0}"#,
            ))
            .await
            .unwrap();
        assert_eq!(patch_res.status(), StatusCode::OK);
        let patched: AdminRoom = serde_json::from_slice(&body_bytes(patch_res).await).unwrap();
        assert_eq!(patched.id, "jogos");
        assert_eq!(patched.title, "Jogatina");
        assert_eq!(patched.position, 0);

        let list_res = app
            .clone()
            .oneshot(rooms_request(Some(INVITE)))
            .await
            .unwrap();
        assert_eq!(list_res.status(), StatusCode::OK);
        let listed: RoomsResponse = serde_json::from_slice(&body_bytes(list_res).await).unwrap();
        assert_eq!(listed.rooms[0].name, "jogos");
        assert_eq!(listed.rooms[0].title, "Jogatina");
        assert_eq!(listed.rooms[0].display_title(), "Jogatina");

        // Missing room on PATCH -> 404.
        let missing_patch = app
            .clone()
            .oneshot(json_request(
                "PATCH",
                "/admin/rooms/nao-existe",
                Some(&admin_auth),
                r#"{"title":"X"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(missing_patch.status(), StatusCode::NOT_FOUND);

        // Delete "afk" -> calls LiveKit delete_room, deletes afk chat only, leaves geral chat intact.
        let del_res = app
            .clone()
            .oneshot(json_request(
                "DELETE",
                "/admin/rooms/afk",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(del_res.status(), StatusCode::NO_CONTENT);
        assert!(
            dir.calls()
                .contains(&ModerationCall::DeleteRoom("afk".to_owned()))
        );
        let afk_hist = store.list_page("afk".to_owned(), None).await.unwrap();
        assert!(afk_hist.messages.is_empty());
        let geral_hist = store.list_page("geral".to_owned(), None).await.unwrap();
        assert_eq!(geral_hist.messages.len(), 1);

        // Missing room on DELETE -> 404.
        let missing_del = app
            .clone()
            .oneshot(json_request(
                "DELETE",
                "/admin/rooms/nao-existe",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(missing_del.status(), StatusCode::NOT_FOUND);

        // Deleting remaining rooms down to 1 blocks deleting the last room (409).
        for id in ["jogos", "mesa-de-cafe", "mesa-de-cafe-2"] {
            let r = app
                .clone()
                .oneshot(json_request(
                    "DELETE",
                    &format!("/admin/rooms/{id}"),
                    Some(&admin_auth),
                    "",
                ))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::NO_CONTENT);
        }
        let last_del = app
            .clone()
            .oneshot(json_request(
                "DELETE",
                "/admin/rooms/geral",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(last_del.status(), StatusCode::CONFLICT);

        // Reaching MAX_ROOMS_COUNT blocks creating a 51st room (409).
        for i in 1..MAX_ROOMS_COUNT {
            let body = serde_json::to_string(&CreateRoomRequest {
                title: format!("Sala {i}"),
            })
            .unwrap();
            let res = app
                .clone()
                .oneshot(json_request(
                    "POST",
                    "/admin/rooms",
                    Some(&admin_auth),
                    &body,
                ))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::CREATED, "room {i}");
        }
        let overflow = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/rooms",
                Some(&admin_auth),
                r#"{"title":"Sala Extra"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(overflow.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn admin_moderation_mute_stop_share_kick_and_move() {
        let config = test_admin_config();
        let dir = Arc::new(
            FakeDirectory::default()
                .with_participants("geral", &["ana", "bia"])
                .with_track("geral", "ana", "TR_mic", TRACK_SOURCE_MICROPHONE)
                .with_track("geral", "ana", "TR_scr", TRACK_SOURCE_SCREEN_SHARE)
                .with_track("geral", "ana", "TR_scra", TRACK_SOURCE_SCREEN_SHARE_AUDIO),
        );
        let store = seeded_store(&config.rooms);
        let app = router(state_with(config, dir.clone(), Some(store), Duration::ZERO));
        let admin_auth = login_admin(&app).await;

        // Mute mic
        let res = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/mute",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(dir.calls().contains(&ModerationCall::MuteTrack {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
            track_sid: "TR_mic".to_owned(),
        }));

        // Stop share: mutes both screen share video & audio tracks AND sends admin.stop_share to ana
        let res = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/stop-share",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let calls = dir.calls();
        assert!(calls.contains(&ModerationCall::MuteTrack {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
            track_sid: "TR_scr".to_owned(),
        }));
        assert!(calls.contains(&ModerationCall::MuteTrack {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
            track_sid: "TR_scra".to_owned(),
        }));
        let stop_msg = calls.iter().find_map(|c| match c {
            ModerationCall::SendData {
                room,
                topic,
                payload,
                destination_identities,
            } if topic == ADMIN_STOP_SHARE_TOPIC => Some((room, payload, destination_identities)),
            _ => None,
        });
        let (room, payload, dests) = stop_msg.expect("admin.stop_share call");
        assert_eq!(room, "geral");
        assert_eq!(dests, &vec!["ana".to_owned()]);
        let decoded: AdminStopShare = serde_json::from_slice(payload).unwrap();
        assert_eq!(decoded, AdminStopShare {});

        // Move participant bia -> jogos
        let move_res = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/rooms/geral/participants/bia/move",
                Some(&admin_auth),
                r#"{"to_room":"jogos"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(move_res.status(), StatusCode::NO_CONTENT);
        let calls = dir.calls();
        let move_msg = calls.iter().find_map(|c| match c {
            ModerationCall::SendData {
                room,
                topic,
                payload,
                destination_identities,
            } if topic == ADMIN_MOVE_TOPIC => Some((room, payload, destination_identities)),
            _ => None,
        });
        let (room, payload, dests) = move_msg.expect("admin.move call");
        assert_eq!(room, "geral");
        assert_eq!(dests, &vec!["bia".to_owned()]);
        let decoded: AdminMove = serde_json::from_slice(payload).unwrap();
        assert_eq!(
            decoded,
            AdminMove {
                to_room: "jogos".to_owned()
            }
        );

        // Move validation & not found checks
        assert_eq!(
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/admin/rooms/geral/participants/bia/move",
                    Some(&admin_auth),
                    r#"{"to_room":"geral"}"#,
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/admin/rooms/geral/participants/bia/move",
                    Some(&admin_auth),
                    r#"{"to_room":"inexistente"}"#,
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/admin/rooms/geral/participants/fantasma/move",
                    Some(&admin_auth),
                    r#"{"to_room":"jogos"}"#,
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );

        // Kick participant
        let kick_res = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/kick",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(kick_res.status(), StatusCode::NO_CONTENT);
        assert!(dir.calls().contains(&ModerationCall::RemoveParticipant {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
        }));
        assert_eq!(
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/admin/rooms/geral/participants/fantasma/kick",
                    Some(&admin_auth),
                    "",
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );

        // Backend moderation failure maps to 502 without leaking internal text
        let failing_app = admin_app_with(
            Arc::new(
                FakeDirectory::default()
                    .with_participants("geral", &["ana"])
                    .failing_moderation(),
            ),
            Some(seeded_store(&test_admin_config().rooms)),
            Duration::ZERO,
        );
        let fail_admin = login_admin(&failing_app).await;
        let fail_res = failing_app
            .oneshot(json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/kick",
                Some(&fail_admin),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(fail_res.status(), StatusCode::BAD_GATEWAY);
        assert!(!error_message(fail_res).await.contains("fake moderation"));

        // SendData failure during stop-share also maps to 502
        let failing_data_app = admin_app_with(
            Arc::new(
                FakeDirectory::default()
                    .with_participants("geral", &["ana"])
                    .failing_send_data(),
            ),
            Some(seeded_store(&test_admin_config().rooms)),
            Duration::ZERO,
        );
        let data_admin = login_admin(&failing_data_app).await;
        let stop_fail = failing_data_app
            .oneshot(json_request(
                "POST",
                "/admin/rooms/geral/participants/ana/stop-share",
                Some(&data_admin),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(stop_fail.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn admin_delete_chat_removes_other_users_message_and_broadcasts() {
        let config = test_admin_config();
        let dir = Arc::new(FakeDirectory::default().with_participants("geral", &["pedro"]));
        let store = seeded_store(&config.rooms);
        let app = router(state_with(config, dir.clone(), Some(store), Duration::ZERO));
        let admin_auth = login_admin(&app).await;
        let pedro_jwt = format!(
            "Bearer {}",
            mint_room_jwt("client-pedro", "Pedro", "geral", true)
        );
        let jogos_jwt = format!(
            "Bearer {}",
            mint_room_jwt("client-pedro", "Pedro", "jogos", true)
        );

        // Post messages in an active room ("geral") and an empty LiveKit room ("jogos").
        assert_eq!(
            app.clone()
                .oneshot(post_chat_req(
                    "geral",
                    Some(&pedro_jwt),
                    &save_chat_body("m-99", "apague isso", 1000),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            app.clone()
                .oneshot(post_chat_req(
                    "jogos",
                    Some(&jogos_jwt),
                    &save_chat_body("m-empty", "sala vazia", 1001),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );

        // Delete active room message -> 204 and broadcasts admin.chat_delete.
        let del = app
            .clone()
            .oneshot(json_request(
                "DELETE",
                "/admin/rooms/geral/chat/m-99",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(del.status(), StatusCode::NO_CONTENT);

        let hist_res = app
            .clone()
            .oneshot(get_chat_req("/rooms/geral/chat", Some(&pedro_jwt)))
            .await
            .unwrap();
        let hist: ChatHistoryResponse =
            serde_json::from_slice(&body_bytes(hist_res).await).unwrap();
        assert!(hist.messages.is_empty());

        let calls = dir.calls();
        let chat_del = calls.iter().find_map(|c| match c {
            ModerationCall::SendData {
                room,
                topic,
                payload,
                destination_identities,
            } if topic == ADMIN_CHAT_DELETE_TOPIC => Some((room, payload, destination_identities)),
            _ => None,
        });
        let (room, payload, dests) = chat_del.expect("admin.chat_delete broadcast");
        assert_eq!(room, "geral");
        assert!(dests.is_empty());
        let decoded: AdminChatDelete = serde_json::from_slice(payload).unwrap();
        assert_eq!(
            decoded,
            AdminChatDelete {
                id: "m-99".to_owned()
            }
        );

        // Delete in inactive LiveKit room STILL returns 204.
        let del_empty = app
            .clone()
            .oneshot(json_request(
                "DELETE",
                "/admin/rooms/jogos/chat/m-empty",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(del_empty.status(), StatusCode::NO_CONTENT);

        // Delete nonexistent message -> 404.
        let del_missing = app
            .clone()
            .oneshot(json_request(
                "DELETE",
                "/admin/rooms/geral/chat/m-missing",
                Some(&admin_auth),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(del_missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_invite_rotation_persists_in_sqlite_and_overrides_env() {
        let mut db_path = std::env::temp_dir();
        db_path.push(format!(
            "zenha-t36a-invite-test-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = test_admin_config();
        let store = Arc::new(ChatStore::open(&db_path).unwrap());
        store.seed_rooms_if_empty_sync(&config.rooms).unwrap();

        let app = router(state_with(
            config.clone(),
            default_fake_dir(),
            Some(store.clone()),
            Duration::ZERO,
        ));
        let admin_auth = login_admin(&app).await;

        // Invalid lengths rejected with 400.
        assert_eq!(
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/admin/invite",
                    Some(&admin_auth),
                    r#"{"new_code":"short"}"#,
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );

        // Rotate invite.
        let rot = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/admin/invite",
                Some(&admin_auth),
                r#"{"new_code":"nova-senha-2026"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(rot.status(), StatusCode::NO_CONTENT);

        // Old env invite now fails on both /token and /rooms.
        assert_eq!(
            app.clone()
                .oneshot(token_request(&token_body("Pedro", "geral", INVITE)))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(rooms_request(Some(INVITE)))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        // New invite succeeds on both /token and /rooms.
        assert_eq!(
            app.clone()
                .oneshot(token_request(&token_body(
                    "Pedro",
                    "geral",
                    "nova-senha-2026"
                )))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(rooms_request(Some("nova-senha-2026")))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // Reopen SQLite file with a fresh server state to prove persistence across restart.
        drop(app);
        drop(store);
        let reopened = Arc::new(ChatStore::open(&db_path).unwrap());
        let app2 = router(state_with(
            config,
            default_fake_dir(),
            Some(reopened),
            Duration::ZERO,
        ));
        assert_eq!(
            app2.clone()
                .oneshot(rooms_request(Some(INVITE)))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app2.oneshot(rooms_request(Some("nova-senha-2026")))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn admin_db_unavailable_returns_503_for_db_routes_while_readonly_rooms_work() {
        let app = admin_app_with(default_fake_dir(), None, Duration::ZERO);
        let admin_auth = login_admin(&app).await;

        // Read-only /rooms falls back to config.rooms.
        let rooms_res = app
            .clone()
            .oneshot(rooms_request(Some(INVITE)))
            .await
            .unwrap();
        assert_eq!(rooms_res.status(), StatusCode::OK);
        let parsed: RoomsResponse = serde_json::from_slice(&body_bytes(rooms_res).await).unwrap();
        assert_eq!(parsed.rooms.len(), 3);
        assert_eq!(parsed.rooms[0].title, "geral");

        // All DB-backed admin routes return 503.
        for req in [
            json_request(
                "POST",
                "/admin/rooms",
                Some(&admin_auth),
                r#"{"title":"Nova"}"#,
            ),
            json_request(
                "PATCH",
                "/admin/rooms/geral",
                Some(&admin_auth),
                r#"{"title":"X"}"#,
            ),
            json_request("DELETE", "/admin/rooms/geral", Some(&admin_auth), ""),
            json_request(
                "DELETE",
                "/admin/rooms/geral/chat/m-1",
                Some(&admin_auth),
                "",
            ),
            json_request(
                "POST",
                "/admin/invite",
                Some(&admin_auth),
                r#"{"new_code":"nova-senha-2026"}"#,
            ),
        ] {
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
    }
}
