//! Room JWT token issuance via `POST /token`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use zenha_protocol::{
    TokenRequest, TokenResponse, validate_client_id, validate_display_name, validate_room_name,
};

use crate::room_access::{key_matches, lock_owners};

use super::{ApiError, AppState, room_records, verify_invite};

pub(super) async fn issue_token(
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
    let rooms = room_records(&state).await?;
    let record = rooms
        .iter()
        .find(|r| r.id == request.room)
        .ok_or(ApiError::RoomNotFound)?;
    let owners = lock_owners(&rooms, record);
    let secret = &state.config.livekit_api_secret;
    let has_access = owners.is_empty()
        || request.room_key.as_deref().is_some_and(|key| {
            owners
                .iter()
                .any(|(owner, hash)| key_matches(secret, owner, hash, key))
        });
    if !has_access {
        return Err(ApiError::RoomLocked);
    }

    let name = request.display_name.trim();
    let token = crate::token::mint(&state.config, &request.client_id, name, &request.room)
        .map_err(|err| {
            tracing::error!(%err, "minting token failed");
            ApiError::TokenFailure
        })?;

    Ok(Json(TokenResponse {
        token,
        livekit_url: state.config.livekit_url.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use axum::Router;
    use axum::http::StatusCode;
    use livekit_api::access_token::TokenVerifier;
    use tower::ServiceExt;
    use zenha_protocol::{ROOM_LOCKED_ERROR, TokenRequest, TokenResponse};

    use crate::config::Config;

    use super::super::router;
    use super::super::test_support::{
        API_KEY, API_SECRET, INVITE, TTL, app, body_bytes, default_fake_dir, error_message,
        locked_app, locked_store, rooms_request, seeded_store, state_with, test_config, token_body,
        token_body_with_id, token_request, unlock,
    };

    fn app_with_config(config: Config) -> Router {
        let chat = Some(seeded_store(&config.rooms));
        router(state_with(config, default_fake_dir(), chat, Duration::ZERO))
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
        assert!(claims.video.can_update_own_metadata());
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
    async fn token_requires_a_valid_key_for_locked_rooms_and_accepts_the_parent_key() {
        let store = locked_store().await;
        let app = locked_app(&store);
        let with_key = |room: &str, key: Option<&str>| {
            serde_json::to_string(&TokenRequest {
                client_id: "client-pedro".to_owned(),
                display_name: "Pedro".to_owned(),
                room: room.to_owned(),
                invite_code: INVITE.to_owned(),
                room_key: key.map(str::to_owned),
            })
            .unwrap()
        };

        for (room, key) in [("geral", None), ("geral", Some("bad")), ("filha", None)] {
            let response = app
                .clone()
                .oneshot(token_request(&with_key(room, key)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{room} {key:?}");
            assert_eq!(error_message(response).await, ROOM_LOCKED_ERROR);
        }

        let key = unlock(&app, "geral", "senha-vip").await.room_key;
        for room in ["geral", "filha"] {
            let response = app
                .clone()
                .oneshot(token_request(&with_key(room, Some(&key))))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{room}");
        }

        let response = app
            .clone()
            .oneshot(token_request(&with_key("jogos", None)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
