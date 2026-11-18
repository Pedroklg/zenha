use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::StatusCode;
use livekit_api::access_token::{AccessToken, VideoGrants};
use tower::ServiceExt;
use zenha_protocol::{
    ChatHistoryResponse, ChatMessage, MAX_CHAT_ID_LEN, MAX_CHAT_MESSAGE_LEN, SavedChatMessage,
    TokenResponse,
};

use crate::store::ChatStore;

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

fn base64url_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3f) as usize] as char);
        }
    }
    out
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for block in padded.chunks_exact(64) {
        let mut w = [0_u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, val) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(val);
        }
    }
    let mut out = [0_u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * (i + 1)].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn sign_hs256_jwt(claims_json: &str, secret: &str) -> String {
    let header = base64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = base64url_encode(claims_json.as_bytes());
    let signing_input = format!("{header}.{payload}");
    let mut key = [0_u8; 64];
    let secret_bytes = secret.as_bytes();
    if secret_bytes.len() > 64 {
        let digest = sha256(secret_bytes);
        key[..32].copy_from_slice(&digest);
    } else {
        key[..secret_bytes.len()].copy_from_slice(secret_bytes);
    }
    let mut ipad = [0x36_u8; 64];
    let mut opad = [0x5c_u8; 64];
    for i in 0..64 {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    let mut inner = ipad.to_vec();
    inner.extend_from_slice(signing_input.as_bytes());
    let inner_hash = sha256(&inner);
    let mut outer = opad.to_vec();
    outer.extend_from_slice(&inner_hash);
    let sig = base64url_encode(&sha256(&outer));
    format!("{signing_input}.{sig}")
}

#[tokio::test]
async fn chat_routes_require_valid_bearer_token() {
    let router = app();
    let expired = sign_hs256_jwt(
        r#"{"exp":1,"iss":"test-key","nbf":0,"sub":"client-a","name":"Ana","video":{"roomJoin":true,"room":"geral"}}"#,
        API_SECRET,
    );
    let empty_sub = sign_hs256_jwt(
        &format!(
            r#"{{"exp":{},"iss":"test-key","nbf":0,"sub":"","name":"Ana","video":{{"roomJoin":true,"room":"geral"}}}}"#,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 600
        ),
        API_SECRET,
    );
    let wrong_secret = AccessToken::with_api_key(API_KEY, "wrong-secret-01234567890123456789")
        .with_ttl(Duration::from_secs(600))
        .with_identity("client-a")
        .with_name("Ana")
        .with_grants(VideoGrants {
            room_join: true,
            room: "geral".to_owned(),
            ..Default::default()
        })
        .to_jwt()
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
