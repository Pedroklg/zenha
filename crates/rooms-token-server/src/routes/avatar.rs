//! Rotas HTTP para envio, leitura e remoção de foto de perfil (`PUT`/`GET`/`DELETE /avatar`).

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use rooms_protocol::{AvatarUploadResponse, validate_avatar_version, validate_client_id};

use crate::avatar::{AvatarProcessError, process_avatar};
use crate::config::Config;

use super::{ApiError, AppState, verify_bearer_claims};

const IMMUTABLE_CACHE_CONTROL: &str = "private, max-age=31536000, immutable";
const NO_STORE_CACHE_CONTROL: &str = "private, no-store";

fn authenticate_any_room(config: &Config, headers: &HeaderMap) -> Result<String, ApiError> {
    let claims = verify_bearer_claims(config, headers)?;
    if !claims.video.room_join {
        return Err(ApiError::ForbiddenRoom);
    }
    validate_client_id(&claims.sub).map_err(|_| ApiError::InvalidToken)?;
    Ok(claims.sub)
}

pub(super) async fn put_avatar(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AvatarUploadResponse>, ApiError> {
    let identity = authenticate_any_room(&state.config, &headers)?;
    let Some(chat) = state.chat.as_ref() else {
        return Err(ApiError::ChatUnavailable);
    };
    if !state.avatar_limiter.check(&identity) {
        return Err(ApiError::RateLimited);
    }

    let raw = body.to_vec();
    let processed = tokio::task::spawn_blocking(move || process_avatar(&raw))
        .await
        .map_err(|err| {
            tracing::error!(%err, "avatar processing task failed");
            ApiError::ChatStoreFailure
        })?
        .map_err(|err| match err {
            AvatarProcessError::TooLarge => ApiError::PayloadTooLarge,
            AvatarProcessError::Empty
            | AvatarProcessError::UnsupportedFormat
            | AvatarProcessError::InvalidDimensions
            | AvatarProcessError::Decode(_) => ApiError::InvalidAvatar(err.to_string()),
            AvatarProcessError::Encode(msg) => {
                tracing::error!(error = %msg, "avatar PNG encoding failed");
                ApiError::ChatStoreFailure
            }
        })?;

    chat.upsert_avatar(identity, processed.png, processed.version.clone())
        .await
        .map_err(|err| {
            tracing::error!(%err, "failed to store avatar");
            ApiError::ChatStoreFailure
        })?;

    Ok(Json(AvatarUploadResponse {
        version: processed.version,
    }))
}

pub(super) async fn delete_avatar(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let identity = authenticate_any_room(&state.config, &headers)?;
    let Some(chat) = state.chat.as_ref() else {
        return Err(ApiError::ChatUnavailable);
    };

    chat.delete_avatar(identity).await.map_err(|err| {
        tracing::error!(%err, "failed to delete avatar");
        ApiError::ChatStoreFailure
    })?;

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn get_avatar(
    State(state): State<AppState>,
    Path(identity): Path<String>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _requester = authenticate_any_room(&state.config, &headers)?;
    validate_client_id(&identity).map_err(|_| ApiError::Invalid("invalid identity"))?;
    let requested_version = parse_version_query(raw_query.as_deref())?;

    let Some(chat) = state.chat.as_ref() else {
        return Err(ApiError::ChatUnavailable);
    };

    let Some((png, version)) = chat.get_avatar(identity).await.map_err(|err| {
        tracing::error!(%err, "failed to fetch avatar");
        ApiError::ChatStoreFailure
    })?
    else {
        return Err(ApiError::AvatarNotFound);
    };

    let cache_control = if requested_version.as_deref() == Some(version.as_str()) {
        IMMUTABLE_CACHE_CONTROL
    } else {
        NO_STORE_CACHE_CONTROL
    };

    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control),
            ),
        ],
        png,
    )
        .into_response())
}

fn parse_version_query(raw_query: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(query) = raw_query else {
        return Ok(None);
    };
    let mut version = None;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair
            .split_once('=')
            .ok_or(ApiError::Invalid("invalid query parameter"))?;
        if key == "v" {
            validate_avatar_version(value)
                .map_err(|_| ApiError::Invalid("invalid avatar version"))?;
            version = Some(value.to_owned());
        }
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use image::{ImageFormat, ImageReader, Rgba, RgbaImage};
    use rooms_protocol::{AVATAR_SIZE_PX, AvatarUploadResponse, MAX_AVATAR_UPLOAD_BYTES};
    use tower::ServiceExt;

    use crate::routes::test_support::{
        app, body_bytes, default_fake_dir, mint_room_jwt, state_with, test_config,
    };
    use crate::routes::{AVATAR_RATE_LIMIT, router};

    fn make_png(width: u32, height: u32, color: Rgba<u8>) -> Vec<u8> {
        let img = RgbaImage::from_pixel(width, height, color);
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    fn put_req(auth: Option<&str>, body: Vec<u8>) -> Request<Body> {
        let mut builder = Request::builder().method("PUT").uri("/avatar");
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::from(body)).unwrap()
    }

    fn get_req(uri: &str, auth: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn delete_req(auth: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("DELETE").uri("/avatar");
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn avatar_put_get_delete_cycle_enforces_jwt_and_normalization() {
        let app = app();
        let ana_jwt = format!(
            "Bearer {}",
            mint_room_jwt("client-ana", "Ana", "geral", true)
        );
        let bia_other_room_jwt =
            format!("Bearer {}", mint_room_jwt("client-bia", "Bia", "afk", true));
        let png = make_png(240, 120, Rgba([10, 150, 220, 255]));

        // Missing or non-room JWT is rejected.
        let res = app
            .clone()
            .oneshot(put_req(None, png.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let no_join = format!(
            "Bearer {}",
            mint_room_jwt("client-ana", "Ana", "geral", false)
        );
        let res = app
            .clone()
            .oneshot(put_req(Some(&no_join), png.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);

        // Absent avatar returns 404.
        let res = app
            .clone()
            .oneshot(get_req("/avatar/client-ana", Some(&bia_other_room_jwt)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // PUT normalizes to 128x128 PNG under claims.sub identity.
        let res = app
            .clone()
            .oneshot(put_req(Some(&ana_jwt), png))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let uploaded: AvatarUploadResponse =
            serde_json::from_slice(&body_bytes(res).await).unwrap();

        // Any room JWT can GET /avatar/{identity}; ?v=<version> enables immutable cache.
        let res = app
            .clone()
            .oneshot(get_req(
                &format!("/avatar/client-ana?v={}", uploaded.version),
                Some(&bia_other_room_jwt),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(
            res.headers()[header::CACHE_CONTROL],
            "private, max-age=31536000, immutable"
        );
        let body = body_bytes(res).await;
        let decoded = ImageReader::new(Cursor::new(&body))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(
            (decoded.width(), decoded.height()),
            (AVATAR_SIZE_PX, AVATAR_SIZE_PX)
        );

        // Without ?v=<version>, Cache-Control is no-store.
        let res = app
            .clone()
            .oneshot(get_req("/avatar/client-ana", Some(&bia_other_room_jwt)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CACHE_CONTROL], "private, no-store");

        // Replacing image changes version.
        let png2 = make_png(64, 64, Rgba([220, 40, 40, 255]));
        let res = app
            .clone()
            .oneshot(put_req(Some(&ana_jwt), png2))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let uploaded2: AvatarUploadResponse =
            serde_json::from_slice(&body_bytes(res).await).unwrap();
        assert_ne!(uploaded2.version, uploaded.version);

        // Bia's upload updates only client-bia, not client-ana.
        let png3 = make_png(32, 32, Rgba([0, 255, 0, 255]));
        let res = app
            .clone()
            .oneshot(put_req(Some(&bia_other_room_jwt), png3))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // DELETE removes caller's avatar (client-ana -> 404, client-bia still present).
        let res = app
            .clone()
            .oneshot(delete_req(Some(&ana_jwt)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

        let res = app
            .clone()
            .oneshot(get_req("/avatar/client-ana", Some(&bia_other_room_jwt)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let res = app
            .oneshot(get_req("/avatar/client-bia", Some(&ana_jwt)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn avatar_rejects_oversized_corrupt_rate_limit_and_missing_db() {
        let app = app();
        let jwt = format!(
            "Bearer {}",
            mint_room_jwt("client-ana", "Ana", "geral", true)
        );

        let too_large = vec![0u8; MAX_AVATAR_UPLOAD_BYTES + 1];
        let res = app
            .clone()
            .oneshot(put_req(Some(&jwt), too_large))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let res = app
            .clone()
            .oneshot(put_req(Some(&jwt), b"corrupt".to_vec()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let png = make_png(32, 32, Rgba([50, 60, 70, 255]));
        let rate_jwt = format!(
            "Bearer {}",
            mint_room_jwt("client-rate", "Rate", "geral", true)
        );
        for _ in 0..AVATAR_RATE_LIMIT {
            let res = app
                .clone()
                .oneshot(put_req(Some(&rate_jwt), png.clone()))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        }
        let res = app.oneshot(put_req(Some(&rate_jwt), png)).await.unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

        // Missing DB returns 503 Service Unavailable.
        let no_db_app = router(state_with(
            test_config(),
            default_fake_dir(),
            None,
            Duration::ZERO,
        ));
        let res = no_db_app
            .oneshot(get_req("/avatar/client-ana", Some(&jwt)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
