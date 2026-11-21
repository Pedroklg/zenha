use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::StatusCode;
use tower::ServiceExt;
use zenha_protocol::{
    ChatHistoryResponse, ChatMessage, MAX_CHAT_ID_LEN, MAX_CHAT_MESSAGE_LEN, SavedChatMessage,
    TokenResponse,
};

use crate::store::ChatStore;
use crate::token::{Claims, VideoGrants, sign_claims};

use super::super::router;
use super::super::test_support::{
    API_KEY, API_SECRET, INVITE, app, body_bytes, default_fake_dir, delete_chat_req, error_message,
    get_chat_req, mint_room_jwt, post_chat_req, save_chat_body, state_with, test_config,
    token_body, token_body_with_id, token_request,
};

fn app_with_store(chat: Option<Arc<ChatStore>>) -> Router {
    router(state_with(
        test_config(),
        default_fake_dir(),
        chat,
        Duration::ZERO,
    ))
}

#[tokio::test]
async fn chat_routes_require_valid_bearer_token() {
    let router = app();
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let room_grants = || VideoGrants {
        room_join: true,
        room: "geral".to_owned(),
        ..VideoGrants::default()
    };
    let expired = sign_claims(
        API_SECRET,
        &Claims {
            iss: API_KEY.to_owned(),
            sub: "client-a".to_owned(),
            nbf: 0,
            exp: 1,
            name: "Ana".to_owned(),
            video: room_grants(),
            ..Claims::default()
        },
    )
    .unwrap();
    let empty_sub = sign_claims(
        API_SECRET,
        &Claims {
            iss: API_KEY.to_owned(),
            sub: String::new(),
            nbf: 0,
            exp: now + 600,
            name: "Ana".to_owned(),
            video: room_grants(),
            ..Claims::default()
        },
    )
    .unwrap();
    let wrong_secret = sign_claims(
        "wrong-secret-01234567890123456789",
        &Claims {
            iss: API_KEY.to_owned(),
            sub: "client-a".to_owned(),
            nbf: now,
            exp: now + 600,
            name: "Ana".to_owned(),
            video: room_grants(),
            ..Claims::default()
        },
    )
    .unwrap();

    let bad_headers: Vec<Option<String>> = vec![
        None,
        Some("Basic abc".to_owned()),
        Some("Bearer   ".to_owned()),
        Some("Bearer not.a.jwt".to_owned()),
        Some(format!("Bearer {wrong_secret}")),
        Some(format!("Bearer {expired}")),
        Some(format!("Bearer {empty_sub}")),
    ];

    for auth in bad_headers {
        let post_res = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                auth.as_deref(),
                &save_chat_body("m1", "oi", 100),
            ))
            .await
            .unwrap();
        assert_eq!(post_res.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
        assert!(!error_message(post_res).await.is_empty());

        let get_res = router
            .clone()
            .oneshot(get_chat_req("/rooms/geral/chat", auth.as_deref()))
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::UNAUTHORIZED, "{auth:?}");

        let del_res = router
            .clone()
            .oneshot(delete_chat_req("geral", "m1", auth.as_deref()))
            .await
            .unwrap();
        assert_eq!(del_res.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
    }
}

#[tokio::test]
async fn chat_routes_reject_wrong_room_or_missing_room_join() {
    let router = app();
    let geral_jwt = mint_room_jwt("client-a", "Ana", "geral", true);
    let no_join_jwt = mint_room_jwt("client-a", "Ana", "jogos", false);

    for jwt in [geral_jwt, no_join_jwt] {
        let auth = format!("Bearer {jwt}");
        let post_res = router
            .clone()
            .oneshot(post_chat_req(
                "jogos",
                Some(&auth),
                &save_chat_body("m1", "oi", 100),
            ))
            .await
            .unwrap();
        assert_eq!(post_res.status(), StatusCode::FORBIDDEN);

        let get_res = router
            .clone()
            .oneshot(get_chat_req("/rooms/jogos/chat", Some(&auth)))
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::FORBIDDEN);

        let del_res = router
            .clone()
            .oneshot(delete_chat_req("jogos", "m1", Some(&auth)))
            .await
            .unwrap();
        assert_eq!(del_res.status(), StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn chat_routes_reject_invalid_input() {
    let router = app();
    let jwt = mint_room_jwt("client-a", "Ana", "geral", true);
    let auth = format!("Bearer {jwt}");

    let bad_room = router
        .clone()
        .oneshot(get_chat_req("/rooms/Sala%20Ruim/chat", Some(&auth)))
        .await
        .unwrap();
    assert_eq!(bad_room.status(), StatusCode::BAD_REQUEST);

    for raw_body in ["not json", ""] {
        let res = router
            .clone()
            .oneshot(post_chat_req("geral", Some(&auth), raw_body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    for bad_id in [
        "",
        &"a".repeat(MAX_CHAT_ID_LEN + 1),
        "bad id",
        "bad!id",
        "é",
    ] {
        let res = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&auth),
                &save_chat_body(bad_id, "oi", 100),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad_id:?}");
    }

    for bad_text in ["", "   \n\t ", &"é".repeat(MAX_CHAT_MESSAGE_LEN + 1)] {
        let res = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&auth),
                &save_chat_body("m-1", bad_text, 100),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    for bad_query in [
        "/rooms/geral/chat?before=",
        "/rooms/geral/chat?before=bad%20id",
        "/rooms/geral/chat?before=%ZZ",
    ] {
        let res = router
            .clone()
            .oneshot(get_chat_req(bad_query, Some(&auth)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad_query}");
    }

    let bad_del_id = router
        .clone()
        .oneshot(delete_chat_req("geral", "bad!id", Some(&auth)))
        .await
        .unwrap();
    assert_eq!(bad_del_id.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn chat_post_enforces_rate_limit_per_client_id() {
    let router = app();
    let ana_auth = format!(
        "Bearer {}",
        mint_room_jwt("client-ana", "Ana", "geral", true)
    );
    let bia_auth = format!(
        "Bearer {}",
        mint_room_jwt("client-bia", "Bia", "geral", true)
    );

    for i in 0..10 {
        let res = router
            .clone()
            .oneshot(post_chat_req(
                "geral",
                Some(&ana_auth),
                &save_chat_body(&format!("m-{i}"), "oi", 1000 + i),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
    }

    let limited = router
        .clone()
        .oneshot(post_chat_req(
            "geral",
            Some(&ana_auth),
            &save_chat_body("m-10", "spam", 1010),
        ))
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(!error_message(limited).await.is_empty());

    let bia_res = router
        .clone()
        .oneshot(post_chat_req(
            "geral",
            Some(&bia_auth),
            &save_chat_body("bia-1", "oi", 1011),
        ))
        .await
        .unwrap();
    assert_eq!(bia_res.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn chat_delete_enforces_ownership_and_missing() {
    let router = app();
    let ana_auth = format!(
        "Bearer {}",
        mint_room_jwt("client-ana", "Ana", "geral", true)
    );
    let bia_auth = format!(
        "Bearer {}",
        mint_room_jwt("client-bia", "Bia", "geral", true)
    );

    let created = router
        .clone()
        .oneshot(post_chat_req(
            "geral",
            Some(&ana_auth),
            &save_chat_body("m-1", "minha mensagem", 1000),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let forbidden = router
        .clone()
        .oneshot(delete_chat_req("geral", "m-1", Some(&bia_auth)))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let missing = router
        .clone()
        .oneshot(delete_chat_req("geral", "does-not-exist", Some(&ana_auth)))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let deleted = router
        .clone()
        .oneshot(delete_chat_req("geral", "m-1", Some(&ana_auth)))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(body_bytes(deleted).await.is_empty());

    let deleted_again = router
        .clone()
        .oneshot(delete_chat_req("geral", "m-1", Some(&ana_auth)))
        .await
        .unwrap();
    assert_eq!(deleted_again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn token_to_post_to_get_uses_token_identity_and_ignores_forged_author() {
    let router = app();

    let tok_body = token_body_with_id("client-ana-01", "  Ana Maria  ", "geral", INVITE);
    let tok_res = router
        .clone()
        .oneshot(token_request(&tok_body))
        .await
        .unwrap();
    assert_eq!(tok_res.status(), StatusCode::OK);
    let TokenResponse { token, .. } = serde_json::from_slice(&body_bytes(tok_res).await).unwrap();
    let ana_auth = format!("Bearer {token}");

    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let far_future = now_ms + 3_600_000;
    let forged_body = format!(
        r#"{{"id":"msg_1","text":"salva aqui","sent_at_unix_ms":{far_future},"sender":"Admin","sender_id":"forged-id","saved":false}}"#
    );

    let post_res = router
        .clone()
        .oneshot(post_chat_req("geral", Some(&ana_auth), &forged_body))
        .await
        .unwrap();
    assert_eq!(post_res.status(), StatusCode::CREATED);
    let posted: ChatMessage = serde_json::from_slice(&body_bytes(post_res).await).unwrap();
    assert_eq!(posted.id, "msg_1");
    assert_eq!(posted.sender, "Ana Maria");
    assert_eq!(posted.text, "salva aqui");
    assert!(posted.saved);
    assert!(posted.sent_at_unix_ms <= now_ms + 65_000);

    let dup_res = router
        .clone()
        .oneshot(post_chat_req(
            "geral",
            Some(&ana_auth),
            &save_chat_body("msg_1", "tentativa 2", now_ms),
        ))
        .await
        .unwrap();
    assert_eq!(dup_res.status(), StatusCode::OK);
    let dup_msg: ChatMessage = serde_json::from_slice(&body_bytes(dup_res).await).unwrap();
    assert_eq!(dup_msg, posted);

    let bia_auth = format!(
        "Bearer {}",
        mint_room_jwt("client-bia", "Bia", "geral", true)
    );
    let conflict_res = router
        .clone()
        .oneshot(post_chat_req(
            "geral",
            Some(&bia_auth),
            &save_chat_body("msg_1", "sequestro", now_ms),
        ))
        .await
        .unwrap();
    assert_eq!(conflict_res.status(), StatusCode::CONFLICT);

    let get_res = router
        .clone()
        .oneshot(get_chat_req("/rooms/geral/chat", Some(&bia_auth)))
        .await
        .unwrap();
    assert_eq!(get_res.status(), StatusCode::OK);
    let history: ChatHistoryResponse = serde_json::from_slice(&body_bytes(get_res).await).unwrap();
    assert_eq!(
        history,
        ChatHistoryResponse {
            messages: vec![SavedChatMessage {
                message: posted,
                sender_id: "client-ana-01".to_owned(),
            }],
            has_more: false,
            webhook_posts: Vec::new(),
        }
    );
}

#[tokio::test]
async fn get_chat_paginates_chronologically_with_before() {
    let store = Arc::new(ChatStore::open_in_memory().unwrap());
    for i in 0..105_u64 {
        store
            .insert_at(
                "geral".to_owned(),
                format!("m-{i:03}"),
                "client-a".to_owned(),
                "Ana".to_owned(),
                format!("msg {i}"),
                1_000 + i,
                2_000 + i,
            )
            .await
            .unwrap();
    }
    let router = app_with_store(Some(store));
    let auth = format!("Bearer {}", mint_room_jwt("client-a", "Ana", "geral", true));

    let page1_res = router
        .clone()
        .oneshot(get_chat_req("/rooms/geral/chat", Some(&auth)))
        .await
        .unwrap();
    assert_eq!(page1_res.status(), StatusCode::OK);
    let page1: ChatHistoryResponse = serde_json::from_slice(&body_bytes(page1_res).await).unwrap();
    assert!(page1.has_more);
    assert_eq!(page1.messages.len(), 100);
    assert_eq!(page1.messages[0].message.id, "m-005");
    assert_eq!(page1.messages[99].message.id, "m-104");

    let page2_res = router
        .clone()
        .oneshot(get_chat_req("/rooms/geral/chat?before=m-005", Some(&auth)))
        .await
        .unwrap();
    assert_eq!(page2_res.status(), StatusCode::OK);
    let page2: ChatHistoryResponse = serde_json::from_slice(&body_bytes(page2_res).await).unwrap();
    assert!(!page2.has_more);
    let ids: Vec<&str> = page2
        .messages
        .iter()
        .map(|m| m.message.id.as_str())
        .collect();
    assert_eq!(ids, vec!["m-000", "m-001", "m-002", "m-003", "m-004"]);
}

#[tokio::test]
async fn chat_returns_503_when_db_unavailable_while_token_works() {
    let missing_dir = PathBuf::from("/nonexistent-zenha-dir-for-test/sub/chat.db");
    assert!(ChatStore::open(&missing_dir).is_err());

    let router = app_with_store(None);
    let tok_body = token_body("Ana", "geral", INVITE);
    let tok_res = router
        .clone()
        .oneshot(token_request(&tok_body))
        .await
        .unwrap();
    assert_eq!(tok_res.status(), StatusCode::OK);
    let TokenResponse { token, .. } = serde_json::from_slice(&body_bytes(tok_res).await).unwrap();
    let auth = format!("Bearer {token}");

    let post_res = router
        .clone()
        .oneshot(post_chat_req(
            "geral",
            Some(&auth),
            &save_chat_body("m-1", "oi", 1000),
        ))
        .await
        .unwrap();
    assert_eq!(post_res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!error_message(post_res).await.is_empty());

    let get_res = router
        .clone()
        .oneshot(get_chat_req("/rooms/geral/chat", Some(&auth)))
        .await
        .unwrap();
    assert_eq!(get_res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let del_res = router
        .clone()
        .oneshot(delete_chat_req("geral", "m-1", Some(&auth)))
        .await
        .unwrap();
    assert_eq!(del_res.status(), StatusCode::SERVICE_UNAVAILABLE);
}
