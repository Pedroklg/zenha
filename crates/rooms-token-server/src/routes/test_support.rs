use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::StatusCode;
use axum::http::{Request, header};
use axum::response::Response;
use http_body_util::BodyExt;
use livekit_api::access_token::{AccessToken, VideoGrants};
use rooms_protocol::{
    ErrorResponse, SaveChatRequest, TokenRequest, UnlockRoomRequest, UnlockRoomResponse,
};
use tower::ServiceExt;

use crate::chat_store::{ChatStore, RoomUpdate};
use crate::config::Config;
use crate::directory::FakeDirectory;

use super::rooms::INVITE_HEADER;
use super::{
    AdminLoginLimiter, AppState, AvatarRateLimiter, ChatRateLimiter, RoomUnlockLimiter, router,
};

pub(super) const INVITE: &str = "s3cret-invite";
pub(super) const ADMIN_PASSWORD: &str = "admin-pass-1234";
pub(super) const API_KEY: &str = "test-key";
pub(super) const API_SECRET: &str = "test-secret-with-enough-length-0123456789";
pub(super) const TTL: u64 = 600;

fn cached_admin_hash() -> String {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| crate::config::hash_secret(ADMIN_PASSWORD).unwrap())
        .clone()
}

pub(super) fn test_config() -> Config {
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

pub(super) fn test_admin_config() -> Config {
    let mut config = test_config();
    config.admin_code_hash = Some(cached_admin_hash());
    config
}

pub(super) fn default_fake_dir() -> Arc<FakeDirectory> {
    Arc::new(
        FakeDirectory::default()
            .with_participants("geral", &["ana", "bia"])
            .with_participants("afk", &["caio"]),
    )
}

pub(super) fn seeded_store(rooms: &[String]) -> Arc<ChatStore> {
    let store = ChatStore::open_in_memory().unwrap();
    store.seed_rooms_if_empty_sync(rooms).unwrap();
    Arc::new(store)
}

pub(super) fn state_with(
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
        avatar_limiter: Arc::new(AvatarRateLimiter::default()),
        admin_login_limiter: Arc::new(AdminLoginLimiter::default()),
        unlock_limiter: Arc::new(RoomUnlockLimiter::default()),
        admin_failure_delay,
    }
}

pub(super) fn app() -> Router {
    let config = test_config();
    let chat = Some(seeded_store(&config.rooms));
    router(state_with(config, default_fake_dir(), chat, Duration::ZERO))
}

pub(super) fn token_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/token")
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

pub(super) fn token_body_with_id(
    client_id: &str,
    display_name: &str,
    room: &str,
    invite: &str,
) -> String {
    serde_json::to_string(&TokenRequest {
        client_id: client_id.to_owned(),
        display_name: display_name.to_owned(),
        room: room.to_owned(),
        invite_code: invite.to_owned(),
        room_key: None,
    })
    .unwrap()
}

pub(super) fn token_body(display_name: &str, room: &str, invite: &str) -> String {
    token_body_with_id("client-pedro", display_name, room, invite)
}

pub(super) fn rooms_request(invite: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri("/rooms");
    if let Some(invite) = invite {
        builder = builder.header(INVITE_HEADER, invite);
    }
    builder.body(Body::empty()).unwrap()
}

pub(super) async fn body_bytes(response: Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

pub(super) async fn error_message(response: Response) -> String {
    let parsed: ErrorResponse = serde_json::from_slice(&body_bytes(response).await).unwrap();
    parsed.error
}

pub(super) fn mint_room_jwt(client_id: &str, name: &str, room: &str, room_join: bool) -> String {
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

pub(super) fn post_chat_req(room: &str, auth: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/rooms/{room}/chat"))
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

pub(super) fn get_chat_req(uri: &str, auth: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    builder.body(Body::empty()).unwrap()
}

pub(super) fn delete_chat_req(room: &str, id: &str, auth: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("DELETE")
        .uri(format!("/rooms/{room}/chat/{id}"));
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    builder.body(Body::empty()).unwrap()
}

pub(super) fn save_chat_body(id: &str, text: &str, sent_at_unix_ms: u64) -> String {
    serde_json::to_string(&SaveChatRequest {
        id: id.to_owned(),
        text: text.to_owned(),
        sent_at_unix_ms,
    })
    .unwrap()
}

pub(super) async fn locked_store() -> Arc<ChatStore> {
    let config = test_config();
    let store = seeded_store(&config.rooms);
    let hash = crate::config::hash_secret("senha-vip").unwrap();
    store
        .create_room("Filha".to_owned(), None, Some("geral".to_owned()))
        .await
        .unwrap();
    store
        .update_room(
            "geral".to_owned(),
            RoomUpdate {
                password_hash: Some(Some(hash)),
                ..RoomUpdate::default()
            },
        )
        .await
        .unwrap();
    store
}

pub(super) fn locked_app(store: &Arc<ChatStore>) -> Router {
    router(state_with(
        test_config(),
        default_fake_dir(),
        Some(store.clone()),
        Duration::ZERO,
    ))
}

pub(super) fn unlock_request(room: &str, invite: &str, password: &str) -> Request<Body> {
    let body = serde_json::to_string(&UnlockRoomRequest {
        invite_code: invite.to_owned(),
        password: password.to_owned(),
    })
    .unwrap();
    Request::builder()
        .method("POST")
        .uri(format!("/rooms/{room}/unlock"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

pub(super) async fn unlock(app: &Router, room: &str, password: &str) -> UnlockRoomResponse {
    let response = app
        .clone()
        .oneshot(unlock_request(room, INVITE, password))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}
