//! `POST /client-reports`: error reports from clients, authenticated by the invite code.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use zenha_protocol::ClientReport;

use crate::store::now_unix_ms;

use super::rooms::INVITE_HEADER;
use super::{ApiError, AppState, verify_invite};

pub(super) async fn post_client_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let provided = headers
        .get(INVITE_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !verify_invite(&state, provided).await? {
        return Err(ApiError::Unauthorized);
    }
    let report: ClientReport =
        serde_json::from_slice(&body).map_err(|_| ApiError::MalformedBody)?;
    report.validate().map_err(ApiError::Invalid)?;
    if !state
        .chat_limiter
        .check(&format!("report:{}", report.client_id))
    {
        return Err(ApiError::RateLimited);
    }
    let chat = state.chat.as_ref().ok_or(ApiError::ChatUnavailable)?;
    let events_json = serde_json::to_string(&report.events).map_err(|err| {
        tracing::error!(%err, "serializing client report failed");
        ApiError::ChatStoreFailure
    })?;
    let client_id = report.client_id.clone();
    let events = report.events.len();
    chat.save_client_report(report, events_json, now_unix_ms())
        .await
        .map_err(|err| {
            tracing::error!(%err, "storing client report failed");
            ApiError::ChatStoreFailure
        })?;
    tracing::info!(client = %client_id, events, "client report stored");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    use super::super::router;
    use super::super::test_support::{
        INVITE, default_fake_dir, seeded_store, state_with, test_config,
    };
    use super::*;

    const REPORT: &str = r#"{"client_id":"abc","display_name":"Ana","version":"0.1.19","os":"Windows","events":[{"at_unix_ms":1,"level":"warn","target":"zenha_core::audio","message":"audio device failed","count":3}]}"#;

    fn app_with_store() -> (Router, std::sync::Arc<crate::store::ChatStore>) {
        let config = test_config();
        let store = seeded_store(&config.rooms);
        let app = router(state_with(
            config,
            default_fake_dir(),
            Some(store.clone()),
            std::time::Duration::ZERO,
        ));
        (app, store)
    }

    fn report_request(invite: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/client-reports")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(invite) = invite {
            builder = builder.header(INVITE_HEADER, invite);
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    #[tokio::test]
    async fn valid_report_is_stored() {
        let (app, _store) = app_with_store();
        let response = app
            .oneshot(report_request(Some(INVITE), REPORT))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn report_needs_the_invite_code() {
        for invite in [None, Some("wrong")] {
            let (app, _store) = app_with_store();
            let response = app.oneshot(report_request(invite, REPORT)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{invite:?}");
        }
    }

    #[tokio::test]
    async fn bad_bodies_are_rejected() {
        let empty_events =
            r#"{"client_id":"abc","display_name":"Ana","version":"1","os":"W","events":[]}"#;
        for (body, status) in [
            ("not json", StatusCode::BAD_REQUEST),
            (empty_events, StatusCode::BAD_REQUEST),
        ] {
            let (app, _store) = app_with_store();
            let response = app
                .oneshot(report_request(Some(INVITE), body))
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{body}");
        }
    }

    #[tokio::test]
    async fn oversized_body_is_refused() {
        let (app, _store) = app_with_store();
        let body = "x".repeat(zenha_protocol::MAX_CLIENT_REPORT_BODY_BYTES + 1);
        let response = app
            .oneshot(report_request(Some(INVITE), &body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
