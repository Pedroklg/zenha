use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rooms_protocol::{
    ErrorResponse, RoomSummary, RoomsResponse, TokenRequest, TokenResponse, validate_display_name,
    validate_room_name,
};
use subtle::ConstantTimeEq;

use crate::config::Config;
use crate::directory::RoomDirectory;
use crate::token;

const INVITE_HEADER: &str = "x-invite-code";

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub directory: Arc<dyn RoomDirectory>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/token", post(issue_token))
        .route("/rooms", get(list_rooms))
        .with_state(state)
}

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("malformed request body")]
    MalformedBody,
    #[error("invalid invite code")]
    Unauthorized,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("could not issue token")]
    TokenFailure,
    #[error("room directory unavailable")]
    DirectoryFailure,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::MalformedBody | Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::TokenFailure => StatusCode::INTERNAL_SERVER_ERROR,
            Self::DirectoryFailure => StatusCode::BAD_GATEWAY,
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

    if !invite_matches(&state.config.invite_code, &request.invite_code) {
        return Err(ApiError::Unauthorized);
    }
    validate_display_name(&request.display_name)
        .map_err(|_| ApiError::Invalid("invalid display name"))?;
    validate_room_name(&request.room).map_err(|_| ApiError::Invalid("invalid room name"))?;

    let identity = request.display_name.trim();
    let token = token::mint(&state.config, identity, &request.room).map_err(|err| {
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
    if !invite_matches(&state.config.invite_code, provided) {
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use livekit_api::access_token::TokenVerifier;
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
            livekit_url: "ws://livekit.test:7880".to_owned(),
            livekit_api_key: API_KEY.to_owned(),
            livekit_api_secret: API_SECRET.to_owned(),
            token_ttl_secs: TTL,
            rooms: vec!["geral".to_owned(), "jogos".to_owned(), "afk".to_owned()],
        }
    }

    fn app_with(directory: FakeDirectory) -> Router {
        router(AppState {
            config: Arc::new(test_config()),
            directory: Arc::new(directory),
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

    fn token_body(display_name: &str, room: &str, invite: &str) -> String {
        serde_json::to_string(&TokenRequest {
            display_name: display_name.to_owned(),
            room: room.to_owned(),
            invite_code: invite.to_owned(),
        })
        .unwrap()
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
    async fn token_success_carries_room_identity_and_expiry() {
        let body = token_body("  Pedro  ", "jogos", INVITE);
        let response = app().oneshot(token_request(&body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let parsed: TokenResponse = serde_json::from_slice(&body_bytes(response).await).unwrap();
        assert_eq!(parsed.livekit_url, "ws://livekit.test:7880");

        let claims = TokenVerifier::with_api_key(API_KEY, API_SECRET)
            .verify(&parsed.token)
            .unwrap();
        assert_eq!(claims.sub, "Pedro");
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
}
