//! Internal LiveKit webhook receiver (`POST /internal/livekit-webhook`).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;

use super::{ApiError, AppState};

#[derive(Deserialize)]
struct LiveKitWebhookEvent {
    #[serde(default)]
    event: String,
    #[serde(default)]
    room: Option<WebhookRoom>,
}

#[derive(Deserialize)]
struct WebhookRoom {
    #[serde(default)]
    name: String,
}

pub(super) async fn handle_livekit_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::InvalidToken)?;
    crate::token::verify_webhook(
        &state.config.livekit_api_key,
        &state.config.livekit_api_secret,
        auth,
        &body,
    )
    .map_err(|_| ApiError::InvalidToken)?;

    let payload: LiveKitWebhookEvent =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;

    let Some(cache) = &state.occupants_cache else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let Some(room_name) = payload.room.map(|r| r.name).filter(|name| !name.is_empty()) else {
        return Ok(StatusCode::NO_CONTENT);
    };

    match payload.event.as_str() {
        "room_finished" | "room_deleted" => {
            cache.clear_room(&room_name);
            tracing::debug!(
                event = %payload.event,
                room = %room_name,
                "cleared room in occupants cache"
            );
        }
        "room_started"
        | "participant_joined"
        | "participant_left"
        | "participant_connection_aborted"
        | "participant_attributes_changed"
        | "track_published"
        | "track_unpublished"
        | "track_muted"
        | "track_unmuted" => match state.directory.participants(&room_name).await {
            Ok(occupants) => {
                cache.update_room(room_name.clone(), occupants);
                tracing::debug!(
                    event = %payload.event,
                    room = %room_name,
                    "refreshed room in occupants cache"
                );
            }
            Err(err) => {
                tracing::warn!(
                    event = %payload.event,
                    room = %room_name,
                    %err,
                    "failed to refresh room on livekit webhook"
                );
            }
        },
        _ => {}
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;
    use zenha_protocol::RoomsResponse;

    use crate::directory::{FakeDirectory, OccupantsCache};
    use crate::token::{Claims, base64_standard_encode, sign_claims};

    use super::super::router;
    use super::super::test_support::{
        API_KEY, API_SECRET, INVITE, body_bytes, default_fake_dir, rooms_request, seeded_store,
        state_with, test_config,
    };

    fn cached_app(dir: Arc<FakeDirectory>, ttl: Duration) -> Router {
        let config = test_config();
        let chat = Some(seeded_store(&config.rooms));
        let mut state = state_with(config, dir, chat, Duration::ZERO);
        state.occupants_cache = Some(Arc::new(OccupantsCache::new(ttl)));
        router(state)
    }

    fn sign_webhook(secret: &str, issuer: &str, body: &[u8], exp_offset: i64) -> String {
        let now = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let digest = Sha256::digest(body);
        sign_claims(
            secret,
            &Claims {
                iss: issuer.to_owned(),
                nbf: now,
                exp: now + exp_offset,
                sha256: Some(base64_standard_encode(&digest)),
                ..Claims::default()
            },
        )
        .unwrap()
    }

    fn webhook_request(auth: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/internal/livekit-webhook")
            .header(header::CONTENT_TYPE, "application/webhook+json");
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    async fn fetch_room_participants(app: &Router, room: &str) -> Vec<String> {
        let res = app
            .clone()
            .oneshot(rooms_request(Some(INVITE)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let parsed: RoomsResponse = serde_json::from_slice(&body_bytes(res).await).unwrap();
        parsed
            .rooms
            .into_iter()
            .find(|r| r.name == room)
            .unwrap()
            .participants
    }

    #[tokio::test]
    async fn webhook_rejects_missing_bad_expired_or_tampered_jwt() {
        let dir = default_fake_dir();
        let app = cached_app(dir.clone(), Duration::from_secs(60));
        let body = r#"{"event":"participant_joined","room":{"name":"geral"}}"#;

        let wrong_secret = sign_webhook("wrong-secret", API_KEY, body.as_bytes(), 300);
        let wrong_issuer = sign_webhook(API_SECRET, "wrong-key", body.as_bytes(), 300);
        let expired = sign_webhook(API_SECRET, API_KEY, body.as_bytes(), -10);
        let for_other_body = sign_webhook(API_SECRET, API_KEY, b"{}", 300);

        for auth in [
            None,
            Some("not-a-jwt"),
            Some(wrong_secret.as_str()),
            Some(wrong_issuer.as_str()),
            Some(expired.as_str()),
            Some(for_other_body.as_str()),
        ] {
            let res = app
                .clone()
                .oneshot(webhook_request(auth, body))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
        }
        assert!(dir.participants_calls().is_empty());
    }

    #[tokio::test]
    async fn webhook_refreshes_only_affected_room_and_rooms_serves_from_cache() {
        let dir = default_fake_dir();
        let app = cached_app(dir.clone(), Duration::from_secs(60));

        // First GET /rooms syncs all 3 rooms once.
        assert_eq!(
            fetch_room_participants(&app, "geral").await,
            vec!["ana".to_owned(), "bia".to_owned()]
        );
        assert_eq!(dir.participants_calls().len(), 3);
        dir.clear_participants_calls();

        // Second GET /rooms serves straight from cache with 0 LiveKit RPCs.
        assert_eq!(
            fetch_room_participants(&app, "geral").await,
            vec!["ana".to_owned(), "bia".to_owned()]
        );
        assert!(dir.participants_calls().is_empty());

        // Simulate participant joining "geral" and webhook arriving.
        dir.set_participants("geral", &["ana", "bia", "duda"]);
        let join_body = r#"{"event":"participant_joined","room":{"name":"geral"}}"#;
        let auth = sign_webhook(API_SECRET, API_KEY, join_body.as_bytes(), 300);
        let res = app
            .clone()
            .oneshot(webhook_request(Some(&auth), join_body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(dir.participants_calls(), vec!["geral".to_owned()]);
        dir.clear_participants_calls();

        // Next GET /rooms sees "duda" with 0 additional LiveKit RPCs.
        assert_eq!(
            fetch_room_participants(&app, "geral").await,
            vec!["ana".to_owned(), "bia".to_owned(), "duda".to_owned()]
        );
        assert!(dir.participants_calls().is_empty());

        // room_finished clears only that room without calling directory.participants.
        let finish_body = r#"{"event":"room_finished","room":{"name":"geral"}}"#;
        let bearer_auth = format!(
            "Bearer {}",
            sign_webhook(API_SECRET, API_KEY, finish_body.as_bytes(), 300)
        );
        let res = app
            .clone()
            .oneshot(webhook_request(Some(&bearer_auth), finish_body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(dir.participants_calls().is_empty());
        assert!(fetch_room_participants(&app, "geral").await.is_empty());
        assert_eq!(
            fetch_room_participants(&app, "afk").await,
            vec!["caio".to_owned()]
        );
        assert!(dir.participants_calls().is_empty());
    }

    #[tokio::test]
    async fn stale_cache_reconciles_all_rooms_from_directory() {
        let dir = default_fake_dir();
        let app = cached_app(dir.clone(), Duration::ZERO);

        assert_eq!(
            fetch_room_participants(&app, "geral").await,
            vec!["ana".to_owned(), "bia".to_owned()]
        );
        assert_eq!(dir.participants_calls().len(), 3);
        dir.clear_participants_calls();

        dir.set_participants("jogos", &["eva"]);
        assert_eq!(
            fetch_room_participants(&app, "jogos").await,
            vec!["eva".to_owned()]
        );
        assert_eq!(dir.participants_calls().len(), 3);
    }
}
