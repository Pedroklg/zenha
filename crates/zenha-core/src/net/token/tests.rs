use std::collections::BTreeMap;

use reqwest::StatusCode;
use zenha_protocol::{ROOM_KEYS_HEADER, ROOM_LOCKED_ERROR, TokenRequest};

use super::errors::{
    admin_login_error, admin_status_error, avatar_status_error, chat_status_error, status_error,
    token_status_error, unlock_status_error,
};
use super::{HttpTokenClient, INVITE_HEADER};
use crate::api::SessionError;

#[test]
fn room_keys_go_in_the_rooms_header_and_the_token_body() {
    let client = HttpTokenClient::new("http://server.invalid/").unwrap();
    let keys = BTreeMap::from([
        ("geral".to_owned(), "k1".to_owned()),
        ("vip".to_owned(), "k2".to_owned()),
    ]);
    let rooms = client.rooms_http_request("convite", &keys).build().unwrap();
    assert_eq!(rooms.url().as_str(), "http://server.invalid/rooms");
    assert_eq!(rooms.headers()[ROOM_KEYS_HEADER], "geral=k1,vip=k2");
    assert_eq!(rooms.headers()[INVITE_HEADER], "convite");
    let plain = client
        .rooms_http_request("convite", &BTreeMap::new())
        .build()
        .unwrap();
    assert!(!plain.headers().contains_key(ROOM_KEYS_HEADER));

    let token = client
        .token_http_request(&TokenRequest {
            client_id: "client-pedro".to_owned(),
            display_name: "Pedro".to_owned(),
            room: "vip".to_owned(),
            invite_code: "convite".to_owned(),
            room_key: Some("k2".to_owned()),
        })
        .build()
        .unwrap();
    let body = token.body().and_then(reqwest::Body::as_bytes).unwrap();
    let sent: TokenRequest = serde_json::from_slice(body).unwrap();
    assert_eq!(sent.room_key.as_deref(), Some("k2"));
}

#[test]
fn room_lock_status_mapping() {
    assert_eq!(
        token_status_error(StatusCode::FORBIDDEN, Some(ROOM_LOCKED_ERROR), "vip"),
        SessionError::RoomLocked("vip".to_owned())
    );
    assert!(matches!(
        token_status_error(StatusCode::FORBIDDEN, Some("other"), "vip"),
        SessionError::TokenServer(_)
    ));
    assert_eq!(
        unlock_status_error(StatusCode::FORBIDDEN),
        SessionError::RoomPasswordRejected
    );
    assert_eq!(
        unlock_status_error(StatusCode::TOO_MANY_REQUESTS),
        SessionError::RoomUnlockRateLimited
    );
    assert_eq!(
        unlock_status_error(StatusCode::UNAUTHORIZED),
        SessionError::InviteRejected
    );
}

#[test]
fn status_mapping() {
    assert_eq!(
        status_error(StatusCode::UNAUTHORIZED),
        SessionError::InviteRejected
    );
    assert_eq!(
        status_error(StatusCode::INTERNAL_SERVER_ERROR),
        SessionError::TokenServer("server returned HTTP 500".to_owned())
    );
}

#[test]
fn chat_status_mapping() {
    assert_eq!(
        chat_status_error(StatusCode::UNAUTHORIZED),
        SessionError::Unauthorized
    );
    let rate_limited = chat_status_error(StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(rate_limited, SessionError::RateLimited);
    assert!(!rate_limited.to_string().contains("429"));
    assert!(matches!(
        chat_status_error(StatusCode::FORBIDDEN),
        SessionError::TokenServer(_)
    ));
    assert!(matches!(
        chat_status_error(StatusCode::NOT_FOUND),
        SessionError::TokenServer(_)
    ));
    assert!(matches!(
        chat_status_error(StatusCode::CONFLICT),
        SessionError::TokenServer(_)
    ));
    assert!(matches!(
        chat_status_error(StatusCode::SERVICE_UNAVAILABLE),
        SessionError::TokenServer(_)
    ));
    assert_eq!(
        chat_status_error(StatusCode::INTERNAL_SERVER_ERROR),
        SessionError::TokenServer("server returned HTTP 500".to_owned())
    );
}

#[test]
fn url_joins_without_double_slash() {
    let client = HttpTokenClient::new("http://host:8080/").map(|c| c.url("/token"));
    assert_eq!(client.as_deref(), Ok("http://host:8080/token"));
}

#[test]
fn chat_urls_build_and_validate() {
    let Ok(client) = HttpTokenClient::new("http://host:8080/") else {
        panic!("valid url should construct client");
    };
    assert_eq!(
        client.chat_url("geral").as_deref(),
        Ok("http://host:8080/rooms/geral/chat")
    );
    assert_eq!(
        client.chat_history_url("geral", None).as_deref(),
        Ok("http://host:8080/rooms/geral/chat")
    );
    assert_eq!(
        client.chat_history_url("geral", Some("msg_9-a")).as_deref(),
        Ok("http://host:8080/rooms/geral/chat?before=msg_9-a")
    );
    assert_eq!(
        client.chat_delete_url("geral", "msg_9-a").as_deref(),
        Ok("http://host:8080/rooms/geral/chat/msg_9-a")
    );
    assert!(client.chat_url("../bad").is_err());
    assert!(client.chat_history_url("geral", Some("a&b")).is_err());
    assert!(client.chat_delete_url("geral", "").is_err());
}

#[test]
fn admin_login_status_mapping_distinguishes_errors() {
    assert_eq!(
        admin_login_error(StatusCode::UNAUTHORIZED, Some("invalid admin password")),
        SessionError::AdminInvalidPassword
    );
    assert_eq!(
        admin_login_error(StatusCode::NOT_FOUND, Some("not found")),
        SessionError::AdminDisabled
    );
    assert_eq!(
        admin_login_error(StatusCode::TOO_MANY_REQUESTS, Some("rate limit exceeded")),
        SessionError::AdminRateLimited
    );
    assert_eq!(
        admin_login_error(StatusCode::BAD_REQUEST, Some("invalid client_id")),
        SessionError::InvalidInput("invalid client_id".to_owned())
    );
    assert_eq!(
        admin_login_error(StatusCode::INTERNAL_SERVER_ERROR, None),
        SessionError::Admin("server returned HTTP 500".to_owned())
    );
}

#[test]
fn admin_action_status_mapping_distinguishes_404_401_403_429() {
    assert_eq!(
        admin_status_error(
            StatusCode::UNAUTHORIZED,
            Some("invalid or missing token"),
            true
        ),
        SessionError::AdminUnauthorized
    );
    assert_eq!(
        admin_status_error(
            StatusCode::FORBIDDEN,
            Some("admin privileges required"),
            true
        ),
        SessionError::AdminForbidden
    );
    assert_eq!(
        admin_status_error(StatusCode::TOO_MANY_REQUESTS, None, false),
        SessionError::AdminRateLimited
    );
    assert_eq!(
        admin_status_error(StatusCode::NOT_FOUND, Some("not found"), true),
        SessionError::AdminDisabled
    );
    assert_eq!(
        admin_status_error(StatusCode::NOT_FOUND, None, false),
        SessionError::AdminDisabled
    );
    assert_eq!(
        admin_status_error(
            StatusCode::NOT_FOUND,
            Some("room or participant not found"),
            true
        ),
        SessionError::AdminTargetNotFound("room or participant not found".to_owned())
    );
    assert_eq!(
        admin_status_error(
            StatusCode::CONFLICT,
            Some("cannot delete the last room"),
            true
        ),
        SessionError::Admin("cannot delete the last room".to_owned())
    );
}

#[test]
fn admin_urls_build_and_validate() {
    let Ok(client) = HttpTokenClient::new("http://host:8080/") else {
        panic!("valid url should construct client");
    };
    assert_eq!(
        client.admin_room_url("geral").as_deref(),
        Ok("http://host:8080/admin/rooms/geral")
    );
    assert_eq!(
        client
            .admin_participant_url("geral", "ana-1", "kick")
            .as_deref(),
        Ok("http://host:8080/admin/rooms/geral/participants/ana-1/kick")
    );
    assert_eq!(
        client.admin_chat_delete_url("geral", "msg_1").as_deref(),
        Ok("http://host:8080/admin/rooms/geral/chat/msg_1")
    );
    assert!(client.admin_room_url("../bad").is_err());
    assert!(
        client
            .admin_participant_url("geral", "../bad", "kick")
            .is_err()
    );
    assert!(client.admin_chat_delete_url("geral", "a/b").is_err());
}

#[test]
fn webhook_urls_build_and_validate() {
    let client = HttpTokenClient::new("http://host:8080/").unwrap();
    assert_eq!(
        client.admin_webhook_url("abc123").as_deref(),
        Ok("http://host:8080/admin/webhooks/abc123")
    );
    assert!(client.admin_webhook_url("../bad").is_err());
    assert!(client.admin_webhook_url("").is_err());
    assert_eq!(
        client.url("/hooks/abc123/tok"),
        "http://host:8080/hooks/abc123/tok"
    );
}

#[test]
fn avatar_urls_and_status_errors_validate() {
    let client = HttpTokenClient::new("http://host:8080/").unwrap();
    assert_eq!(
        client
            .avatar_get_url("client-ana", Some("0123456789abcdef"))
            .as_deref(),
        Ok("http://host:8080/avatar/client-ana?v=0123456789abcdef")
    );
    assert_eq!(
        client.avatar_get_url("client-ana", None).as_deref(),
        Ok("http://host:8080/avatar/client-ana")
    );
    assert!(client.avatar_get_url("../bad", None).is_err());
    assert!(client.avatar_get_url("client-ana", Some("../bad")).is_err());

    assert_eq!(
        avatar_status_error(StatusCode::UNAUTHORIZED, None),
        SessionError::Unauthorized
    );
    assert_eq!(
        avatar_status_error(StatusCode::TOO_MANY_REQUESTS, None),
        SessionError::RateLimited
    );
    assert_eq!(
        avatar_status_error(StatusCode::PAYLOAD_TOO_LARGE, Some("too big")),
        SessionError::Avatar("too big".to_owned())
    );
    assert_eq!(
        avatar_status_error(StatusCode::BAD_REQUEST, Some("bad format")),
        SessionError::Avatar("bad format".to_owned())
    );
}
