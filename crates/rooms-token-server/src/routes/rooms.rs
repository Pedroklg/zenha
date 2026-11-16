//! Listagem de salas e ocupantes via `GET /rooms`.

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use rooms_protocol::{RoomSummary, RoomsResponse};

use super::{ApiError, AppState, verify_invite};

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::http::StatusCode;
    use rooms_protocol::{RoomSummary, RoomsResponse};
    use tower::ServiceExt;

    use crate::directory::FakeDirectory;

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
}
