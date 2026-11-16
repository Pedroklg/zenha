//! Rotas administrativas: login, CRUD de salas, moderação, exclusão de chat e rotação de convite.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use rooms_protocol::{
    ADMIN_CHAT_DELETE_TOPIC, ADMIN_MOVE_TOPIC, ADMIN_STOP_SHARE_TOPIC, AdminChatDelete,
    AdminLoginRequest, AdminLoginResponse, AdminMove, AdminRoom, AdminStopShare, CreateRoomRequest,
    MoveParticipantRequest, ROOMS_CHANGED_TOPIC, RoomsChanged, UpdateInviteRequest,
    UpdateRoomRequest, validate_client_id, validate_invite_code, validate_room_name,
    validate_room_title,
};

use crate::chat_store::{CreateRoomOutcome, DeleteRoomCheck, DeleteRoomOutcome};
use crate::directory::ModerationError;

use super::chat::validate_chat_id;
use super::{ApiError, AppState, authenticate_admin, room_is_listed, verify_password_hash};

pub const DEFAULT_ADMIN_FAILURE_DELAY: Duration = Duration::from_millis(250);

pub(super) struct OptionalPeerIp(Option<IpAddr>);

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

fn validate_room_and_identity(room: &str, identity: &str) -> Result<(), ApiError> {
    validate_room_name(room).map_err(|_| ApiError::Invalid("invalid room name"))?;
    validate_client_id(identity).map_err(|_| ApiError::Invalid("invalid participant identity"))?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use axum::Router;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Request, StatusCode, header};
    use livekit_api::access_token::TokenVerifier;
    use rooms_protocol::{
        ADMIN_CHAT_DELETE_TOPIC, ADMIN_MOVE_TOPIC, ADMIN_STOP_SHARE_TOPIC, AdminChatDelete,
        AdminLoginRequest, AdminLoginResponse, AdminMove, AdminRoom, AdminStopShare,
        ChatHistoryResponse, CreateRoomRequest, MAX_ROOMS_COUNT, ROOMS_CHANGED_TOPIC,
        RoomsResponse, validate_room_name,
    };
    use tower::ServiceExt;

    use crate::chat_store::ChatStore;
    use crate::directory::{
        FakeDirectory, ModerationCall, TRACK_SOURCE_MICROPHONE, TRACK_SOURCE_SCREEN_SHARE,
        TRACK_SOURCE_SCREEN_SHARE_AUDIO,
    };

    use super::super::router;
    use super::super::test_support::{
        ADMIN_PASSWORD, API_KEY, API_SECRET, INVITE, TTL, app, body_bytes, default_fake_dir,
        delete_chat_req, error_message, get_chat_req, mint_room_jwt, post_chat_req, rooms_request,
        save_chat_body, seeded_store, state_with, test_admin_config, token_body, token_request,
    };

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
        assert!(crate::token::is_admin_claims(&claims));
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
