use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use livekit_api::access_token::TokenVerifier;
use rooms_protocol::{
    ChatHistoryResponse, ChatMessage, ErrorResponse, MAX_CHAT_ID_LEN, RoomSummary, RoomsResponse,
    SaveChatRequest, TokenRequest, TokenResponse, validate_chat_text, validate_client_id,
    validate_display_name, validate_room_name,
};
use subtle::ConstantTimeEq;

use crate::chat_store::{ChatStore, DeleteOutcome, InsertOutcome};
use crate::config::Config;
use crate::directory::RoomDirectory;
use crate::token;

const INVITE_HEADER: &str = "x-invite-code";
const CHAT_RATE_LIMIT: usize = 10;
const CHAT_RATE_WINDOW: Duration = Duration::from_secs(10);
const RATE_LIMIT_PRUNE_THRESHOLD: usize = 1024;

#[derive(Default)]
pub struct ChatRateLimiter {
    entries: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl ChatRateLimiter {
    pub fn check(&self, client_id: &str) -> bool {
        self.check_at(client_id, Instant::now())
    }

    fn check_at(&self, client_id: &str, now: Instant) -> bool {
        let mut map = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if map.len() >= RATE_LIMIT_PRUNE_THRESHOLD {
            map.retain(|_, times| {
                prune_expired(times, now);
                !times.is_empty()
            });
        }
        let times = map.entry(client_id.to_owned()).or_default();
        prune_expired(times, now);
        if times.len() >= CHAT_RATE_LIMIT {
            return false;
        }
        times.push_back(now);
        true
    }
}

fn prune_expired(times: &mut VecDeque<Instant>, now: Instant) {
    while let Some(&front) = times.front() {
        if now.saturating_duration_since(front) >= CHAT_RATE_WINDOW {
            times.pop_front();
        } else {
            break;
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub directory: Arc<dyn RoomDirectory>,
    pub chat: Option<Arc<ChatStore>>,
    pub chat_limiter: Arc<ChatRateLimiter>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/token", post(issue_token))
        .route("/rooms", get(list_rooms))
        .route("/rooms/{room}/chat", get(list_chat).post(post_chat))
        .route("/rooms/{room}/chat/{id}", delete(delete_chat))
        .with_state(state)
}

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("malformed request body")]
    MalformedBody,
    #[error("invalid invite code")]
    Unauthorized,
    #[error("invalid or missing token")]
    InvalidToken,
    #[error("token does not grant access to this room")]
    ForbiddenRoom,
    #[error("cannot delete another user's message")]
    ForbiddenMessage,
    #[error("message not found")]
    MessageNotFound,
    #[error("message id already exists")]
    MessageConflict,
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("could not issue token")]
    TokenFailure,
    #[error("room directory unavailable")]
    DirectoryFailure,
    #[error("chat storage unavailable")]
    ChatUnavailable,
    #[error("chat storage failure")]
    ChatStoreFailure,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::MalformedBody | Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized | Self::InvalidToken => StatusCode::UNAUTHORIZED,
            Self::ForbiddenRoom | Self::ForbiddenMessage => StatusCode::FORBIDDEN,
            Self::MessageNotFound => StatusCode::NOT_FOUND,
            Self::MessageConflict => StatusCode::CONFLICT,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::TokenFailure | Self::ChatStoreFailure => StatusCode::INTERNAL_SERVER_ERROR,
            Self::DirectoryFailure => StatusCode::BAD_GATEWAY,
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

    if !verify_invite(&state.config, &request.invite_code) {
        return Err(ApiError::Unauthorized);
    }
    validate_client_id(&request.client_id).map_err(|_| ApiError::Invalid("invalid client id"))?;
    validate_display_name(&request.display_name)
        .map_err(|_| ApiError::Invalid("invalid display name"))?;
    validate_room_name(&request.room).map_err(|_| ApiError::Invalid("invalid room name"))?;

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
    if !verify_invite(&state.config, provided) {
        return Err(ApiError::Unauthorized);
    }

    let mut rooms = Vec::with_capacity(state.config.rooms.len());
    for name in &state.config.rooms {
        let participants = state.directory.participants(name).await.map_err(|err| {
            tracing::warn!(room = %name, %err, "room lookup failed");
            ApiError::DirectoryFailure
        })?;
        rooms.push(RoomSummary {
            name: name.clone(),
            participants,
        });
    }
    Ok(Json(RoomsResponse { rooms }))
}

struct RoomClaims {
    sender_id: String,
    sender_name: String,
}

fn authenticate_room(
    config: &Config,
    headers: &HeaderMap,
    room: &str,
) -> Result<RoomClaims, ApiError> {
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

    validate_room_name(room).map_err(|_| ApiError::Invalid("invalid room name"))?;
    if !claims.video.room_join || claims.video.room != room {
        return Err(ApiError::ForbiddenRoom);
    }

    Ok(RoomClaims {
        sender_id: claims.sub,
        sender_name: claims.name,
    })
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

fn verify_invite(config: &Config, provided: &str) -> bool {
    if let Some(hash) = &config.invite_code_hash {
        PasswordHash::new(hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(provided.as_bytes(), &parsed)
                .is_ok()
        })
    } else {
        invite_matches(&config.invite_code, provided)
    }
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use livekit_api::access_token::{AccessToken, TokenVerifier, VideoGrants};
    use rooms_protocol::{MAX_CHAT_MESSAGE_LEN, SavedChatMessage};
    use tower::ServiceExt;

    use super::*;
    use crate::directory::FakeDirectory;

    const INVITE: &str = "s3cret-invite";
    const API_KEY: &str = "test-key";
    const API_SECRET: &str = "test-secret-with-enough-length-0123456789";
    const TTL: u64 = 600;

    fn test_config() -> Config {
        Config {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            invite_code: INVITE.to_owned(),
            invite_code_hash: None,
            livekit_url: "ws://livekit.test:7880".to_owned(),
            livekit_api_key: API_KEY.to_owned(),
            livekit_api_secret: API_SECRET.to_owned(),
            token_ttl_secs: TTL,
            rooms: vec!["geral".to_owned(), "jogos".to_owned(), "afk".to_owned()],
            chat_db_path: PathBuf::from("chat.db"),
        }
    }

    fn app_with(directory: FakeDirectory) -> Router {
        router(AppState {
            config: Arc::new(test_config()),
            directory: Arc::new(directory),
            chat: Some(Arc::new(ChatStore::open_in_memory().unwrap())),
            chat_limiter: Arc::new(ChatRateLimiter::default()),
        })
    }

    fn app_with_config(config: Config) -> Router {
        router(AppState {
            config: Arc::new(config),
            directory: Arc::new(
                FakeDirectory::default()
                    .with_participants("geral", &["ana", "bia"])
                    .with_participants("afk", &["caio"]),
            ),
            chat: Some(Arc::new(ChatStore::open_in_memory().unwrap())),
            chat_limiter: Arc::new(ChatRateLimiter::default()),
        })
    }

    fn app_with_store(chat: Option<Arc<ChatStore>>) -> Router {
        router(AppState {
            config: Arc::new(test_config()),
            directory: Arc::new(
                FakeDirectory::default()
                    .with_participants("geral", &["ana", "bia"])
                    .with_participants("afk", &["caio"]),
            ),
            chat,
            chat_limiter: Arc::new(ChatRateLimiter::default()),
        })
    }

    fn app() -> Router {
        app_with(
            FakeDirectory::default()
                .with_participants("geral", &["ana", "bia"])
                .with_participants("afk", &["caio"]),
        )
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
}
