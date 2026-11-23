use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use tower::ServiceExt;
use zenha_protocol::{
    AdminWebhook, ChatHistoryResponse, CreateWebhookRequest, CreatedWebhook,
    MAX_WEBHOOK_BODY_BYTES, MAX_WEBHOOKS_COUNT, WEBHOOK_TOPIC, WebhookPost, WebhooksResponse,
};

use crate::directory::{FakeDirectory, ModerationCall};
use crate::store::{ChatStore, token_hash};

use super::super::router;
use super::super::test_support::{
    body_bytes, error_message, get_chat_req, mint_room_jwt, seeded_store, state_with,
    test_admin_config,
};

const LOBBY: &str = r#"{"embeds":[{"title":"Clique aqui para abrir a lobby","color":"2391737","url":"https://gamersclub.com.br/j/123/abc","fields":[{"name":"Tipo da sala:","value":"FECHADA"},{"name":"Membros:","value":"ana | 15 | KDR: 1.2 \n"}]}]}"#;
const MATCH: &str = r#"{"embeds":[{"color":"2391737","fields":[{"name":"IP da partida:","value":"connect 1.2.3.4:27015;password xyz \n[Conectar ao servidor](steam://connect/1.2.3.4:27015/xyz)"},{"name":"Mapa:","value":"de_mirage"}],"image":{"url":"https://example.com/m.png"}}]}"#;
const TEST: &str = r#"{"embeds":[{"title":"Gamers Club Booster","color":"4961603","fields":[{"name":"Status","value":"OK"}]}]}"#;

const WEBHOOK_ID: &str = "0123456789abcdef0123456789abcdef";
const WEBHOOK_TOKEN: &str = "t0k3n";

struct Fixture {
    app: Router,
    store: Arc<ChatStore>,
    dir: Arc<FakeDirectory>,
}

fn fixture_without_webhook(dir: FakeDirectory) -> Fixture {
    let config = test_admin_config();
    let store = seeded_store(&config.rooms);
    let dir = Arc::new(dir);
    let app = router(state_with(
        config,
        dir.clone(),
        Some(store.clone()),
        Duration::ZERO,
    ));
    Fixture { app, store, dir }
}

async fn add_webhook(f: &Fixture, id: &str, room: &str, token: &str) {
    f.store
        .create_webhook(
            id.to_owned(),
            room.to_owned(),
            "Gamers Club".to_owned(),
            token_hash(token),
        )
        .await
        .unwrap();
}

async fn fixture_with(dir: FakeDirectory) -> Fixture {
    let f = fixture_without_webhook(dir);
    add_webhook(&f, WEBHOOK_ID, "geral", WEBHOOK_TOKEN).await;
    f
}

async fn fixture() -> Fixture {
    fixture_with(FakeDirectory::default().with_participants("geral", &["ana", "bia"])).await
}

fn hook_request(id: &str, token: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/hooks/{id}/{token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn allow_origin(response: &Response) -> Option<&str> {
    response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn valid_token_delivers_to_the_whole_room() {
    for body in [LOBBY, MATCH, TEST] {
        let f = fixture().await;
        let response = f
            .app
            .clone()
            .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(allow_origin(&response), Some("*"));
        let calls = f.dir.calls();
        let [
            ModerationCall::SendData {
                room,
                topic,
                payload,
                destination_identities,
            },
        ] = calls.as_slice()
        else {
            panic!("expected one send_data call, got {calls:?}");
        };
        assert_eq!((room.as_str(), topic.as_str()), ("geral", WEBHOOK_TOPIC));
        assert!(destination_identities.is_empty());
        let post: WebhookPost = serde_json::from_slice(payload).unwrap();
        assert_eq!(post.name, "Gamers Club");
        assert_eq!(post.validate(), Ok(()));
        assert_eq!(post.id.len(), 32);
    }
}

#[tokio::test]
async fn repeated_lobby_link_is_delivered_once() {
    let f = fixture().await;
    for _ in 0..2 {
        let response = f
            .app
            .clone()
            .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, LOBBY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    assert_eq!(f.dir.calls().len(), 1);
}

async fn room_history(app: &Router, uri: &str) -> ChatHistoryResponse {
    let auth = format!(
        "Bearer {}",
        mint_room_jwt("client-pedro", "Pedro", "geral", true)
    );
    let response = app
        .clone()
        .oneshot(get_chat_req(uri, Some(&auth)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

#[tokio::test]
async fn delivered_posts_show_up_in_the_first_history_page() {
    let f = fixture_with(FakeDirectory::default()).await;
    for body in [LOBBY, MATCH] {
        let response = f
            .app
            .clone()
            .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let history = room_history(&f.app, "/rooms/geral/chat").await;
    assert_eq!(history.webhook_posts.len(), 2);
    assert_eq!(
        history.webhook_posts[0].embeds[0].title.as_deref(),
        Some("Clique aqui para abrir a lobby")
    );
    assert!(
        history
            .webhook_posts
            .iter()
            .all(|p| p.name == "Gamers Club")
    );

    let older = room_history(&f.app, "/rooms/geral/chat?before=abc").await;
    assert!(older.webhook_posts.is_empty());
}

#[tokio::test]
async fn wrong_token_and_unknown_id_look_the_same() {
    let f = fixture().await;
    let wrong = f
        .app
        .clone()
        .oneshot(hook_request(WEBHOOK_ID, "nope", TEST))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::NOT_FOUND);
    assert_eq!(allow_origin(&wrong), Some("*"));
    let wrong_msg = error_message(wrong).await;
    let unknown = f
        .app
        .clone()
        .oneshot(hook_request(
            "ffffffffffffffffffffffffffffffff",
            "nope",
            TEST,
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_message(unknown).await, wrong_msg);
    let bad_id = f
        .app
        .clone()
        .oneshot(hook_request("not-valid", WEBHOOK_TOKEN, TEST))
        .await
        .unwrap();
    assert_eq!(bad_id.status(), StatusCode::NOT_FOUND);
    assert!(f.dir.calls().is_empty());
}

#[tokio::test]
async fn revoked_webhook_is_not_found() {
    let f = fixture().await;
    assert!(f.store.delete_webhook(WEBHOOK_ID.to_owned()).await.unwrap());
    let response = f
        .app
        .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, TEST))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(f.dir.calls().is_empty());
}

#[tokio::test]
async fn webhook_of_a_missing_room_is_not_found() {
    let f = fixture().await;
    let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    add_webhook(&f, id, "fantasma", "x").await;
    let response = f.app.oneshot(hook_request(id, "x", TEST)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(f.dir.calls().is_empty());
}

#[tokio::test]
async fn bodies_over_the_limit_are_rejected() {
    let f = fixture().await;
    let big = format!(r#"{{"content":"{}"}}"#, "a".repeat(MAX_WEBHOOK_BODY_BYTES));
    let response = f
        .app
        .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, &big))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(allow_origin(&response), Some("*"));
    assert!(f.dir.calls().is_empty());
}

#[tokio::test]
async fn malformed_and_empty_bodies_are_bad_requests() {
    let f = fixture().await;
    for body in ["not json", r#"{"content":"   ","embeds":[{}]}"#, "{}"] {
        let response = f
            .app
            .clone()
            .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(allow_origin(&response), Some("*"));
    }
    assert!(f.dir.calls().is_empty());
}

#[tokio::test]
async fn empty_room_still_answers_no_content() {
    let f = fixture().await;
    let id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    add_webhook(&f, id, "jogos", "y").await;
    let response = f.app.oneshot(hook_request(id, "y", TEST)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(f.dir.calls().is_empty());
}

#[tokio::test]
async fn livekit_failure_is_a_bad_gateway() {
    let f = fixture_with(
        FakeDirectory::default()
            .with_participants("geral", &["ana"])
            .failing_send_data(),
    )
    .await;
    let response = f
        .app
        .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, TEST))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(allow_origin(&response), Some("*"));
}

#[tokio::test]
async fn deliveries_are_rate_limited_per_webhook() {
    let f = fixture().await;
    for _ in 0..10 {
        let response = f
            .app
            .clone()
            .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, TEST))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let response = f
        .app
        .oneshot(hook_request(WEBHOOK_ID, WEBHOOK_TOKEN, TEST))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(allow_origin(&response), Some("*"));
}

#[tokio::test]
async fn preflight_allows_cross_origin_posts() {
    let f = fixture().await;
    let request = Request::builder()
        .method("OPTIONS")
        .uri(format!("/hooks/{WEBHOOK_ID}/{WEBHOOK_TOKEN}"))
        .header(header::ORIGIN, "https://gamersclub.com.br")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .body(Body::empty())
        .unwrap();
    let response = f.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let headers = response.headers();
    let get = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    assert_eq!(get(header::ACCESS_CONTROL_ALLOW_ORIGIN), Some("*"));
    assert_eq!(
        get(header::ACCESS_CONTROL_ALLOW_METHODS),
        Some("POST, OPTIONS")
    );
    assert_eq!(
        get(header::ACCESS_CONTROL_ALLOW_HEADERS),
        Some("content-type")
    );
    assert_eq!(get(header::ACCESS_CONTROL_MAX_AGE), Some("86400"));
}

fn admin_request(method: &str, uri: &str, auth: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(auth) = auth {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

fn admin_auth() -> String {
    let (token, _) = crate::token::mint_admin(&test_admin_config(), "admin-client").unwrap();
    format!("Bearer {token}")
}

fn create_body(room: &str, name: &str) -> String {
    serde_json::to_string(&CreateWebhookRequest {
        room: room.to_owned(),
        name: name.to_owned(),
    })
    .unwrap()
}

async fn create_webhook(app: &Router, room: &str, name: &str) -> Response {
    app.clone()
        .oneshot(admin_request(
            "POST",
            "/admin/webhooks",
            Some(&admin_auth()),
            &create_body(room, name),
        ))
        .await
        .unwrap()
}

async fn list_webhooks(app: &Router) -> Vec<AdminWebhook> {
    let response = app
        .clone()
        .oneshot(admin_request(
            "GET",
            "/admin/webhooks",
            Some(&admin_auth()),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let parsed: WebhooksResponse = serde_json::from_slice(&body_bytes(response).await).unwrap();
    parsed.webhooks
}

fn empty_fixture() -> Fixture {
    fixture_without_webhook(FakeDirectory::default().with_participants("geral", &["ana"]))
}

#[tokio::test]
async fn admin_webhook_routes_require_an_admin_token() {
    let f = fixture().await;
    let member = format!(
        "Bearer {}",
        mint_room_jwt("client-pedro", "Pedro", "geral", true)
    );
    for auth in [None, Some(member.as_str())] {
        for (method, uri) in [
            ("GET", "/admin/webhooks"),
            ("POST", "/admin/webhooks"),
            ("DELETE", "/admin/webhooks/abc"),
        ] {
            let response = f
                .app
                .clone()
                .oneshot(admin_request(
                    method,
                    uri,
                    auth,
                    &create_body("geral", "GC"),
                ))
                .await
                .unwrap();
            let expected = if auth.is_some() {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            };
            assert_eq!(response.status(), expected, "{method} {uri}");
        }
    }
    assert_eq!(f.store.list_webhooks().await.unwrap().len(), 1);
}

#[tokio::test]
async fn create_list_use_and_delete_a_webhook() {
    let f = empty_fixture();
    let response = create_webhook(&f.app, "geral", "  Gamers Club  ").await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: CreatedWebhook = serde_json::from_slice(&body_bytes(response).await).unwrap();
    assert_eq!(created.webhook.name, "Gamers Club");
    assert_eq!(created.webhook.room, "geral");
    assert_eq!(created.webhook.id.len(), 32);
    let prefix = format!("/hooks/{}/", created.webhook.id);
    let token = created.url.strip_prefix(&prefix).unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

    assert_eq!(list_webhooks(&f.app).await, vec![created.webhook.clone()]);
    let stored = f
        .store
        .get_webhook(created.webhook.id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(stored.token_hash, token);
    assert_eq!(stored.token_hash, token_hash(token));

    let delivered = f
        .app
        .clone()
        .oneshot(hook_request(&created.webhook.id, token, TEST))
        .await
        .unwrap();
    assert_eq!(delivered.status(), StatusCode::NO_CONTENT);

    let uri = format!("/admin/webhooks/{}", created.webhook.id);
    let deleted = f
        .app
        .clone()
        .oneshot(admin_request("DELETE", &uri, Some(&admin_auth()), ""))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let again = f
        .app
        .clone()
        .oneshot(admin_request("DELETE", &uri, Some(&admin_auth()), ""))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
    assert!(list_webhooks(&f.app).await.is_empty());
    let revoked = f
        .app
        .oneshot(hook_request(&created.webhook.id, token, TEST))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_rejects_bad_input_and_unknown_rooms() {
    let f = empty_fixture();
    assert_eq!(
        create_webhook(&f.app, "fantasma", "GC").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        create_webhook(&f.app, "geral", "   ").await.status(),
        StatusCode::BAD_REQUEST
    );
    let malformed = f
        .app
        .clone()
        .oneshot(admin_request(
            "POST",
            "/admin/webhooks",
            Some(&admin_auth()),
            "nope",
        ))
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let bad_id = f
        .app
        .oneshot(admin_request(
            "DELETE",
            "/admin/webhooks/bad-id",
            Some(&admin_auth()),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(bad_id.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhook_count_is_limited() {
    let f = empty_fixture();
    for _ in 0..MAX_WEBHOOKS_COUNT {
        assert_eq!(
            create_webhook(&f.app, "geral", "GC").await.status(),
            StatusCode::CREATED
        );
    }
    assert_eq!(
        create_webhook(&f.app, "geral", "GC").await.status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn deleting_a_room_removes_its_webhooks() {
    let f = empty_fixture();
    assert_eq!(
        create_webhook(&f.app, "jogos", "GC").await.status(),
        StatusCode::CREATED
    );
    assert_eq!(
        create_webhook(&f.app, "geral", "GC").await.status(),
        StatusCode::CREATED
    );
    let deleted = f
        .app
        .clone()
        .oneshot(admin_request(
            "DELETE",
            "/admin/rooms/jogos",
            Some(&admin_auth()),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let left = list_webhooks(&f.app).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].room, "geral");
}

const MATCH_WITH_LINK: &str = r#"{"embeds":[{"color":"2391737","fields":[{"name":"IP da partida:","value":"connect 1.2.3.4:27015;password xyz"},{"name":"Link da partida","value":"https://gamersclub.com.br/lobby/partida/28333771"}]}]}"#;

fn personal_hook_issue_req(invite: &str, client_id: &str) -> Request<Body> {
    let body = serde_json::to_string(&zenha_protocol::PersonalHookRequest {
        invite_code: invite.to_owned(),
        client_id: client_id.to_owned(),
    })
    .unwrap();
    Request::builder()
        .method("POST")
        .uri("/personal-hook")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn personal_hook_post(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn fetch_personal_path(app: &Router, client_id: &str) -> String {
    let response = app
        .clone()
        .oneshot(personal_hook_issue_req(
            super::super::test_support::INVITE,
            client_id,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let parsed: zenha_protocol::PersonalHookResponse =
        serde_json::from_slice(&body_bytes(response).await).unwrap();
    parsed.path
}

async fn room_history_for(app: &Router, room: &str) -> ChatHistoryResponse {
    let auth = format!(
        "Bearer {}",
        mint_room_jwt("client-pedro", "Pedro", room, true)
    );
    let response = app
        .clone()
        .oneshot(get_chat_req(&format!("/rooms/{room}/chat"), Some(&auth)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

#[tokio::test]
async fn personal_hook_endpoint_requires_valid_invite_and_client_id() {
    let f = empty_fixture();
    let wrong = f
        .app
        .clone()
        .oneshot(personal_hook_issue_req("wrong", "client-ana"))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let bad_id = f
        .app
        .clone()
        .oneshot(personal_hook_issue_req(
            super::super::test_support::INVITE,
            "Bad_Id",
        ))
        .await
        .unwrap();
    assert_eq!(bad_id.status(), StatusCode::BAD_REQUEST);

    let path = fetch_personal_path(&f.app, "client-ana").await;
    let sig = path.strip_prefix("/hooks/u/client-ana/").unwrap();
    assert_eq!(sig.len(), 32);
    assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn personal_hook_valid_signature_delivers_and_stores_in_current_room() {
    let f = fixture_without_webhook(
        FakeDirectory::default()
            .with_participants("geral", &["client-ana"])
            .with_participants("jogos", &["client-bia"]),
    );
    let path = fetch_personal_path(&f.app, "client-ana").await;

    let response = f
        .app
        .clone()
        .oneshot(personal_hook_post(&path, LOBBY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(allow_origin(&response), Some("*"));

    let calls = f.dir.calls();
    let [
        ModerationCall::SendData {
            room,
            topic,
            payload,
            destination_identities,
        },
    ] = calls.as_slice()
    else {
        panic!("expected one send_data call, got {calls:?}");
    };
    assert_eq!((room.as_str(), topic.as_str()), ("geral", WEBHOOK_TOPIC));
    assert!(destination_identities.is_empty());
    let post: WebhookPost = serde_json::from_slice(payload).unwrap();
    assert_eq!(post.name, "Gamers Club");
    assert_eq!(post.validate(), Ok(()));

    let geral_history = room_history_for(&f.app, "geral").await;
    assert_eq!(geral_history.webhook_posts.len(), 1);
    let jogos_history = room_history_for(&f.app, "jogos").await;
    assert!(jogos_history.webhook_posts.is_empty());

    // Moving to another room delivers the next post there.
    f.dir.set_participants("geral", &[]);
    f.dir
        .set_participants("jogos", &["client-ana", "client-bia"]);
    let moved = f
        .app
        .clone()
        .oneshot(personal_hook_post(&path, TEST))
        .await
        .unwrap();
    assert_eq!(moved.status(), StatusCode::NO_CONTENT);
    let calls = f.dir.calls();
    assert_eq!(calls.len(), 2);
    assert!(matches!(
        &calls[1],
        ModerationCall::SendData { room, .. } if room == "jogos"
    ));
}

#[tokio::test]
async fn personal_hook_invalid_signature_or_identity_returns_same_not_found() {
    let f = fixture().await;
    let fixed_not_found = f
        .app
        .clone()
        .oneshot(hook_request(WEBHOOK_ID, "wrong", TEST))
        .await
        .unwrap();
    assert_eq!(fixed_not_found.status(), StatusCode::NOT_FOUND);
    let expected_msg = error_message(fixed_not_found).await;

    for uri in [
        "/hooks/u/ana/00000000000000000000000000000000",
        "/hooks/u/Bad_Id/00000000000000000000000000000000",
    ] {
        let res = f
            .app
            .clone()
            .oneshot(personal_hook_post(uri, TEST))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(allow_origin(&res), Some("*"));
        assert_eq!(error_message(res).await, expected_msg);
    }
    assert!(f.dir.calls().is_empty());
}

#[tokio::test]
async fn personal_hook_when_not_in_any_room_skips_delivery_and_storage() {
    let f = fixture_without_webhook(FakeDirectory::default().with_participants("geral", &["bia"]));
    let path = fetch_personal_path(&f.app, "client-ana").await;

    let response = f
        .app
        .clone()
        .oneshot(personal_hook_post(&path, LOBBY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(allow_origin(&response), Some("*"));
    assert!(f.dir.calls().is_empty());
    for room in ["geral", "jogos", "afk"] {
        assert!(
            room_history_for(&f.app, room)
                .await
                .webhook_posts
                .is_empty()
        );
    }
}

#[tokio::test]
async fn personal_hook_deduplicates_same_room_but_allows_another_room() {
    let f = fixture_without_webhook(
        FakeDirectory::default()
            .with_participants("geral", &["client-ana", "client-bia"])
            .with_participants("jogos", &["client-caio"]),
    );
    let ana_path = fetch_personal_path(&f.app, "client-ana").await;
    let bia_path = fetch_personal_path(&f.app, "client-bia").await;
    let caio_path = fetch_personal_path(&f.app, "client-caio").await;

    for (path, body) in [
        (&ana_path, LOBBY),
        (&bia_path, LOBBY),
        (&caio_path, LOBBY),
        (&ana_path, MATCH_WITH_LINK),
        (&bia_path, MATCH_WITH_LINK),
        (&caio_path, MATCH_WITH_LINK),
    ] {
        let res = f
            .app
            .clone()
            .oneshot(personal_hook_post(path, body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    let delivered_rooms: Vec<String> = f
        .dir
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            ModerationCall::SendData { room, .. } => Some(room),
            _ => None,
        })
        .collect();
    assert_eq!(delivered_rooms, vec!["geral", "jogos", "geral", "jogos"]);
    assert_eq!(
        room_history_for(&f.app, "geral").await.webhook_posts.len(),
        2
    );
    assert_eq!(
        room_history_for(&f.app, "jogos").await.webhook_posts.len(),
        2
    );
}

#[tokio::test]
async fn personal_hook_preflight_rate_limit_and_body_limit() {
    let f = fixture_without_webhook(
        FakeDirectory::default().with_participants("geral", &["client-ana"]),
    );
    let path = fetch_personal_path(&f.app, "client-ana").await;

    let preflight = Request::builder()
        .method("OPTIONS")
        .uri(&path)
        .header(header::ORIGIN, "https://gamersclub.com.br")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .body(Body::empty())
        .unwrap();
    let res = f.app.clone().oneshot(preflight).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(allow_origin(&res), Some("*"));

    let big = format!(r#"{{"content":"{}"}}"#, "a".repeat(MAX_WEBHOOK_BODY_BYTES));
    let too_big = f
        .app
        .clone()
        .oneshot(personal_hook_post(&path, &big))
        .await
        .unwrap();
    assert_eq!(too_big.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(allow_origin(&too_big), Some("*"));

    for _ in 0..10 {
        let ok = f
            .app
            .clone()
            .oneshot(personal_hook_post(&path, TEST))
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::NO_CONTENT);
    }
    let limited = f
        .app
        .clone()
        .oneshot(personal_hook_post(&path, TEST))
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(allow_origin(&limited), Some("*"));
}

#[tokio::test]
async fn personal_hook_uses_occupants_cache_hint_and_falls_back_when_participant_moved() {
    let config = test_admin_config();
    let store = seeded_store(&config.rooms);
    let dir = Arc::new(FakeDirectory::default().with_participants("jogos", &["client-ana"]));
    let cache = Arc::new(crate::directory::OccupantsCache::default());
    cache.update_room(
        "geral".to_owned(),
        vec![crate::directory::Occupant {
            identity: "client-ana".to_owned(),
            name: "Ana".to_owned(),
            voice: zenha_protocol::VoiceState::default(),
        }],
    );
    let mut state = state_with(config, dir.clone(), Some(store), Duration::ZERO);
    state.occupants_cache = Some(cache);
    let app = router(state);
    let path = fetch_personal_path(&app, "client-ana").await;

    let res = app
        .clone()
        .oneshot(personal_hook_post(&path, TEST))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let calls = dir.calls();
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        &calls[0],
        ModerationCall::SendData { room, .. } if room == "jogos"
    ));
}
