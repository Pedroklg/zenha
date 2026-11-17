//! Listagem de salas e ocupantes via `GET /rooms` e desbloqueio de sala com senha.

use std::collections::HashMap;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use zenha_protocol::{
    ROOM_KEYS_HEADER, RoomSummary, RoomsResponse, UnlockRoomRequest, UnlockRoomResponse,
    parse_room_keys, validate_room_name,
};

use crate::room_access::{key_matches, lock_owners, room_key};

use super::admin::{OptionalPeerIp, resolved_client_ip};
use super::{ApiError, AppState, room_records, verify_invite, verify_password_hash};

pub(super) const INVITE_HEADER: &str = "x-invite-code";

pub(super) async fn list_rooms(
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

    let keys: HashMap<String, String> = headers
        .get(ROOM_KEYS_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(parse_room_keys)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let secret = &state.config.livekit_api_secret;

    let records = room_records(&state).await?;
    let mut rooms = Vec::with_capacity(records.len());
    for record in &records {
        let mut participants = state
            .directory
            .participants(&record.id)
            .await
            .map_err(|err| {
                tracing::warn!(room = %record.id, %err, "room lookup failed");
                ApiError::DirectoryFailure
            })?;
        let owners = lock_owners(&records, record);
        let locked = !owners.is_empty();
        let unlocked = locked
            && owners.iter().any(|(owner, hash)| {
                keys.get(*owner)
                    .is_some_and(|key| key_matches(secret, owner, hash, key))
            });
        let participant_count = participants.len();
        if locked && !unlocked {
            participants.clear();
        }
        rooms.push(RoomSummary {
            name: record.id.clone(),
            title: record.title.clone(),
            participants,
            participant_count,
            locked,
            unlocked,
            parent: record.parent.clone(),
        });
    }
    Ok(Json(RoomsResponse { rooms }))
}

pub(super) async fn unlock_room(
    State(state): State<AppState>,
    Path(room): Path<String>,
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<UnlockRoomResponse>, ApiError> {
    let request: UnlockRoomRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    if !verify_invite(&state, &request.invite_code).await? {
        return Err(ApiError::Unauthorized);
    }
    validate_room_name(&room).map_err(|_| ApiError::Invalid("invalid room name"))?;
    let records = room_records(&state).await?;
    let record = records
        .iter()
        .find(|r| r.id == room)
        .ok_or(ApiError::RoomNotFound)?;
    let owners: Vec<(String, String)> = lock_owners(&records, record)
        .into_iter()
        .map(|(owner, hash)| (owner.to_owned(), hash.to_owned()))
        .collect();
    if owners.is_empty() {
        return Err(ApiError::Invalid("room has no password"));
    }

    let limit_keys = vec![match resolved_client_ip(peer_ip, &headers) {
        Some(ip) => format!("ip:{ip}"),
        None => format!("room:{room}"),
    }];
    if !state.unlock_limiter.is_allowed(&limit_keys) {
        tracing::warn!(%room, "room unlock rate limited");
        return Err(ApiError::RateLimited);
    }

    for (owner, hash) in owners {
        if verify_password_hash(hash.clone(), request.password.clone()).await {
            let room_key = room_key(&state.config.livekit_api_secret, &owner, &hash);
            return Ok(Json(UnlockRoomResponse {
                room: owner,
                room_key,
            }));
        }
    }
    state.unlock_limiter.record(&limit_keys);
    tracing::warn!(%room, "room unlock failed");
    if !state.admin_failure_delay.is_zero() {
        tokio::time::sleep(state.admin_failure_delay).await;
    }
    Err(ApiError::WrongRoomPassword)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::http::StatusCode;
    use zenha_protocol::{ROOM_KEYS_HEADER, RoomSummary, RoomsResponse, encode_room_keys};
    use tower::ServiceExt;

    use crate::chat_store::RoomUpdate;
    use crate::directory::FakeDirectory;

    use super::super::test_support::{locked_app, locked_store, unlock, unlock_request};

    use super::super::router;
    use super::super::test_support::{
        INVITE, app, body_bytes, error_message, rooms_request, seeded_store, state_with,
        test_config,
    };

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
            participant_count: people.len(),
            ..RoomSummary::default()
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

    async fn rooms_with_keys(app: &Router, keys: &[(&str, &str)]) -> Vec<RoomSummary> {
        let mut request = rooms_request(Some(INVITE));
        request.headers_mut().insert(
            ROOM_KEYS_HEADER,
            encode_room_keys(keys.iter().copied()).parse().unwrap(),
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let parsed: RoomsResponse = serde_json::from_slice(&body_bytes(response).await).unwrap();
        parsed.rooms
    }

    #[tokio::test]
    async fn unlock_checks_invite_and_password_and_returns_the_owner_key() {
        let store = locked_store().await;
        let app = locked_app(&store);

        let response = app
            .clone()
            .oneshot(unlock_request("geral", "wrong", "senha-vip"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(unlock_request("geral", INVITE, "errada"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!error_message(response).await.contains("senha-vip"));

        for (room, status) in [
            ("jogos", StatusCode::BAD_REQUEST),
            ("nada", StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(unlock_request(room, INVITE, "senha-vip"))
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{room}");
        }

        let parent = unlock(&app, "geral", "senha-vip").await;
        assert_eq!(parent.room, "geral");
        let via_child = unlock(&app, "filha", "senha-vip").await;
        assert_eq!(via_child, parent);
    }

    #[tokio::test]
    async fn unlock_is_rate_limited_after_repeated_failures() {
        let store = locked_store().await;
        let app = locked_app(&store);
        let mut last = StatusCode::OK;
        for _ in 0..6 {
            last = app
                .clone()
                .oneshot(unlock_request("geral", INVITE, "errada"))
                .await
                .unwrap()
                .status();
        }
        assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn rooms_hide_occupants_of_locked_rooms_until_a_valid_key_is_sent() {
        let store = locked_store().await;
        let app = locked_app(&store);

        let rooms = rooms_with_keys(&app, &[]).await;
        let geral = &rooms[0];
        assert!(geral.locked && !geral.unlocked);
        assert!(geral.participants.is_empty());
        assert_eq!(geral.occupant_count(), 2);
        let filha = rooms.iter().find(|r| r.name == "filha").unwrap();
        assert!(filha.locked);
        assert_eq!(filha.parent.as_deref(), Some("geral"));
        let afk = rooms.iter().find(|r| r.name == "afk").unwrap();
        assert!(!afk.locked);
        assert_eq!(afk.participants, vec!["caio".to_owned()]);

        let key = unlock(&app, "geral", "senha-vip").await.room_key;
        for keys in [vec![("geral", "bad")], vec![("jogos", key.as_str())]] {
            let rooms = rooms_with_keys(&app, &keys).await;
            assert!(rooms[0].participants.is_empty(), "{keys:?}");
        }

        let rooms = rooms_with_keys(&app, &[("geral", &key)]).await;
        assert!(rooms[0].unlocked);
        assert_eq!(
            rooms[0].participants,
            vec!["ana".to_owned(), "bia".to_owned()]
        );
        assert!(rooms.iter().find(|r| r.name == "filha").unwrap().unlocked);
    }

    #[tokio::test]
    async fn changing_the_password_invalidates_old_keys() {
        let store = locked_store().await;
        let app = locked_app(&store);
        let key = unlock(&app, "geral", "senha-vip").await.room_key;

        let hash = crate::config::hash_secret("senha-nova").unwrap();
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

        let rooms = rooms_with_keys(&app, &[("geral", &key)]).await;
        assert!(!rooms[0].unlocked);
        let fresh = unlock(&app, "geral", "senha-nova").await.room_key;
        assert_ne!(fresh, key);
    }
}
