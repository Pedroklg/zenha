use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;
use zenha_protocol::{
    AdminRoom, CHAT_HISTORY_PAGE, ChatMessage, MAX_ROOM_NAME_LEN, MAX_ROOMS_COUNT,
};

use super::messages::clamp_sent_at;
use super::rooms::{slugify_room_title, unique_room_id};
use super::*;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let n = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("zenha-chat-test-{}-{n}.db", std::process::id()));
        Self { path }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let wal = PathBuf::from(format!("{}-wal", self.path.display()));
        let shm = PathBuf::from(format!("{}-shm", self.path.display()));
        let _ = std::fs::remove_file(wal);
        let _ = std::fs::remove_file(shm);
    }
}

#[test]
fn clamp_sent_at_limits_future_timestamps() {
    assert_eq!(clamp_sent_at(1_000, 2_000), 1_000);
    assert_eq!(clamp_sent_at(62_000, 2_000), 62_000);
    assert_eq!(clamp_sent_at(120_000, 2_000), 62_000);
    assert_eq!(clamp_sent_at(u64::MAX, 2_000), 62_000);
}

#[tokio::test]
async fn file_db_enables_wal_and_persists_across_reopen() {
    let temp = TempDb::new();
    {
        let store = ChatStore::open(&temp.path).unwrap();
        let mode: String = store
            .with_conn(|conn| Ok(conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");

        let outcome = store
            .insert(
                "geral".to_owned(),
                "m-1".to_owned(),
                "client-ana".to_owned(),
                "Ana".to_owned(),
                "olá".to_owned(),
                1_700_000_000_000,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, InsertOutcome::Created(_)));
    }

    let reopened = ChatStore::open(&temp.path).unwrap();
    let page = reopened.list_page("geral".to_owned(), None).await.unwrap();
    assert!(!page.has_more);
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].message.id, "m-1");
    assert_eq!(page.messages[0].message.text, "olá");
    assert!(page.messages[0].message.saved);
    assert_eq!(page.messages[0].sender_id, "client-ana");
}

#[tokio::test]
async fn insert_ignores_duplicate_id_and_separates_rooms() {
    let store = ChatStore::open_in_memory().unwrap();
    let first = store
        .insert(
            "geral".to_owned(),
            "dup-1".to_owned(),
            "client-a".to_owned(),
            "Ana".to_owned(),
            "primeira".to_owned(),
            1000,
        )
        .await
        .unwrap();
    assert_eq!(
        first,
        InsertOutcome::Created(ChatMessage {
            id: "dup-1".to_owned(),
            sender: "Ana".to_owned(),
            sent_at_unix_ms: 1000,
            text: "primeira".to_owned(),
            saved: true,
        })
    );

    let same_author = store
        .insert(
            "geral".to_owned(),
            "dup-1".to_owned(),
            "client-a".to_owned(),
            "Outro Nome".to_owned(),
            "alterada".to_owned(),
            2000,
        )
        .await
        .unwrap();
    assert_eq!(
        same_author,
        InsertOutcome::DuplicateBySameSender(ChatMessage {
            id: "dup-1".to_owned(),
            sender: "Ana".to_owned(),
            sent_at_unix_ms: 1000,
            text: "primeira".to_owned(),
            saved: true,
        })
    );

    let other_author = store
        .insert(
            "geral".to_owned(),
            "dup-1".to_owned(),
            "client-b".to_owned(),
            "Bia".to_owned(),
            "sequestro".to_owned(),
            3000,
        )
        .await
        .unwrap();
    assert_eq!(other_author, InsertOutcome::ConflictOtherSender);

    let other_room = store
        .insert(
            "jogos".to_owned(),
            "dup-1".to_owned(),
            "client-b".to_owned(),
            "Bia".to_owned(),
            "outra sala".to_owned(),
            3000,
        )
        .await
        .unwrap();
    assert!(matches!(other_room, InsertOutcome::Created(_)));
}

#[tokio::test]
async fn pagination_returns_pages_of_100_with_has_more_and_before() {
    let store = ChatStore::open_in_memory().unwrap();
    for i in 0..150_u64 {
        let id = format!("msg-{i:03}");
        let outcome = store
            .insert_at(
                "geral".to_owned(),
                id.clone(),
                "client-a".to_owned(),
                "Ana".to_owned(),
                format!("texto {i}"),
                10_000 + i,
                20_000 + i,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, InsertOutcome::Created(_)));
    }

    let page1 = store.list_page("geral".to_owned(), None).await.unwrap();
    assert!(page1.has_more);
    assert_eq!(page1.messages.len(), CHAT_HISTORY_PAGE);
    assert_eq!(page1.messages[0].message.id, "msg-050");
    assert_eq!(page1.messages[99].message.id, "msg-149");

    let page2 = store
        .list_page("geral".to_owned(), Some("msg-050".to_owned()))
        .await
        .unwrap();
    assert!(!page2.has_more);
    assert_eq!(page2.messages.len(), 50);
    assert_eq!(page2.messages[0].message.id, "msg-000");
    assert_eq!(page2.messages[49].message.id, "msg-049");

    let empty_before_oldest = store
        .list_page("geral".to_owned(), Some("msg-000".to_owned()))
        .await
        .unwrap();
    assert!(!empty_before_oldest.has_more);
    assert!(empty_before_oldest.messages.is_empty());

    let missing_before = store
        .list_page("geral".to_owned(), Some("missing".to_owned()))
        .await
        .unwrap();
    assert!(!missing_before.has_more);
    assert!(missing_before.messages.is_empty());
}

#[tokio::test]
async fn same_millisecond_ties_break_stably_by_id() {
    let store = ChatStore::open_in_memory().unwrap();
    for id in ["b", "a", "c"] {
        store
            .insert_at(
                "geral".to_owned(),
                id.to_owned(),
                "client-a".to_owned(),
                "Ana".to_owned(),
                id.to_owned(),
                5000,
                5000,
            )
            .await
            .unwrap();
    }
    store
        .insert_at(
            "geral".to_owned(),
            "d".to_owned(),
            "client-a".to_owned(),
            "Ana".to_owned(),
            "d".to_owned(),
            5001,
            5001,
        )
        .await
        .unwrap();

    let all = store.list_page("geral".to_owned(), None).await.unwrap();
    let ids: Vec<&str> = all.messages.iter().map(|m| m.message.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "c", "d"]);

    let before_c = store
        .list_page("geral".to_owned(), Some("c".to_owned()))
        .await
        .unwrap();
    let before_ids: Vec<&str> = before_c
        .messages
        .iter()
        .map(|m| m.message.id.as_str())
        .collect();
    assert_eq!(before_ids, vec!["a", "b"]);
}

#[tokio::test]
async fn delete_enforces_owner_and_room() {
    let store = ChatStore::open_in_memory().unwrap();
    store
        .insert(
            "geral".to_owned(),
            "m-1".to_owned(),
            "client-a".to_owned(),
            "Ana".to_owned(),
            "oi".to_owned(),
            1000,
        )
        .await
        .unwrap();

    assert_eq!(
        store
            .delete("geral".to_owned(), "m-1".to_owned(), "client-b".to_owned())
            .await
            .unwrap(),
        DeleteOutcome::Forbidden
    );
    assert_eq!(
        store
            .delete("jogos".to_owned(), "m-1".to_owned(), "client-a".to_owned())
            .await
            .unwrap(),
        DeleteOutcome::NotFound
    );
    assert_eq!(
        store
            .delete(
                "geral".to_owned(),
                "missing".to_owned(),
                "client-a".to_owned()
            )
            .await
            .unwrap(),
        DeleteOutcome::NotFound
    );
    assert_eq!(
        store
            .delete("geral".to_owned(), "m-1".to_owned(), "client-a".to_owned())
            .await
            .unwrap(),
        DeleteOutcome::Deleted
    );
    assert_eq!(
        store
            .delete("geral".to_owned(), "m-1".to_owned(), "client-a".to_owned())
            .await
            .unwrap(),
        DeleteOutcome::NotFound
    );
    let page = store.list_page("geral".to_owned(), None).await.unwrap();
    assert!(page.messages.is_empty());
}

#[tokio::test]
async fn rooms_seed_only_when_empty_and_preserve_admin_changes() {
    let store = ChatStore::open_in_memory().unwrap();
    let seed = vec!["geral".to_owned(), "jogos".to_owned(), "afk".to_owned()];
    store.seed_rooms_if_empty_sync(&seed).unwrap();

    let rooms = store.list_rooms().await.unwrap();
    assert_eq!(
        rooms,
        vec![
            AdminRoom {
                id: "geral".to_owned(),
                title: "geral".to_owned(),
                position: 0,
                ..AdminRoom::default()
            },
            AdminRoom {
                id: "jogos".to_owned(),
                title: "jogos".to_owned(),
                position: 1,
                ..AdminRoom::default()
            },
            AdminRoom {
                id: "afk".to_owned(),
                title: "afk".to_owned(),
                position: 2,
                ..AdminRoom::default()
            },
        ]
    );

    store
        .update_room(
            "geral".to_owned(),
            RoomUpdate {
                title: Some("Sala Geral".to_owned()),
                ..RoomUpdate::default()
            },
        )
        .await
        .unwrap();
    store
        .seed_rooms_if_empty_sync(&["outra".to_owned()])
        .unwrap();
    let after = store.list_rooms().await.unwrap();
    assert_eq!(after.len(), 3);
    assert_eq!(after[0].title, "Sala Geral");
}

#[test]
fn slugify_and_unique_room_id_produce_valid_room_names() {
    use zenha_protocol::validate_room_name;

    for (title, expected) in [
        ("Sala Geral", "sala-geral"),
        ("Bate-papo & Música!", "bate-papo-musica"),
        ("  Café_2  ", "cafe_2"),
        ("🎮🔥", "sala"),
        ("---", "sala"),
    ] {
        let slug = slugify_room_title(title);
        assert_eq!(slug, expected, "{title:?}");
        assert_eq!(validate_room_name(&slug), Ok(()), "{slug:?}");
    }

    let long_slug = slugify_room_title(&"a".repeat(50));
    assert_eq!(long_slug.len(), MAX_ROOM_NAME_LEN);
    assert_eq!(validate_room_name(&long_slug), Ok(()));

    let existing = vec![long_slug.clone(), "sala".to_owned()];
    let dedup_long = unique_room_id(&"a".repeat(50), &existing);
    assert!(dedup_long.len() <= MAX_ROOM_NAME_LEN);
    assert!(dedup_long.ends_with("-2"));
    assert_eq!(validate_room_name(&dedup_long), Ok(()));

    let dedup_fallback = unique_room_id("🎮", &existing);
    assert_eq!(dedup_fallback, "sala-2");
    assert_eq!(validate_room_name(&dedup_fallback), Ok(()));
}

#[tokio::test]
async fn rooms_crud_enforces_limit_reorders_and_cleans_deleted_chat() {
    let store = ChatStore::open_in_memory().unwrap();
    store
        .seed_rooms_if_empty_sync(&["geral".to_owned(), "jogos".to_owned()])
        .unwrap();

    let created = store
        .create_room("Música & Voz".to_owned(), None, None)
        .await
        .unwrap();
    assert_eq!(
        created,
        CreateRoomOutcome::Created(AdminRoom {
            id: "musica-voz".to_owned(),
            title: "Música & Voz".to_owned(),
            position: 2,
            ..AdminRoom::default()
        })
    );

    let UpdateRoomOutcome::Updated(moved) = store
        .update_room(
            "musica-voz".to_owned(),
            RoomUpdate {
                title: Some("Música".to_owned()),
                position: Some(0),
                ..RoomUpdate::default()
            },
        )
        .await
        .unwrap()
    else {
        panic!("room should be updated");
    };
    assert_eq!(moved.position, 0);
    assert_eq!(moved.title, "Música");

    let ordered: Vec<String> = store
        .list_rooms()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ordered, vec!["musica-voz", "geral", "jogos"]);

    store
        .insert(
            "jogos".to_owned(),
            "chat-1".to_owned(),
            "client-a".to_owned(),
            "Ana".to_owned(),
            "bora".to_owned(),
            1000,
        )
        .await
        .unwrap();

    assert_eq!(
        store
            .check_can_delete_room("jogos".to_owned())
            .await
            .unwrap(),
        DeleteRoomCheck::Ready
    );
    assert_eq!(
        store.commit_delete_room("jogos".to_owned()).await.unwrap(),
        DeleteRoomOutcome::Deleted
    );
    assert!(
        store
            .list_page("jogos".to_owned(), None)
            .await
            .unwrap()
            .messages
            .is_empty()
    );

    assert_eq!(
        store.commit_delete_room("geral".to_owned()).await.unwrap(),
        DeleteRoomOutcome::Deleted
    );
    assert_eq!(
        store
            .check_can_delete_room("musica-voz".to_owned())
            .await
            .unwrap(),
        DeleteRoomCheck::LastRoom
    );
    assert_eq!(
        store
            .commit_delete_room("musica-voz".to_owned())
            .await
            .unwrap(),
        DeleteRoomOutcome::LastRoom
    );

    for i in 1..MAX_ROOMS_COUNT {
        let outcome = store
            .create_room(format!("Sala {i}"), None, None)
            .await
            .unwrap();
        assert!(matches!(outcome, CreateRoomOutcome::Created(_)));
    }
    assert_eq!(
        store
            .create_room("Extra".to_owned(), None, None)
            .await
            .unwrap(),
        CreateRoomOutcome::LimitReached
    );
}

#[tokio::test]
async fn admin_delete_removes_any_message_and_invite_override_persists() {
    let temp = TempDb::new();
    {
        let store = ChatStore::open(&temp.path).unwrap();
        store
            .insert(
                "geral".to_owned(),
                "m-1".to_owned(),
                "client-ana".to_owned(),
                "Ana".to_owned(),
                "olá".to_owned(),
                1000,
            )
            .await
            .unwrap();
        assert!(
            store
                .delete_as_admin("geral".to_owned(), "m-1".to_owned())
                .await
                .unwrap()
        );
        assert!(
            !store
                .delete_as_admin("geral".to_owned(), "m-1".to_owned())
                .await
                .unwrap()
        );
        assert_eq!(store.get_invite_code_hash().await.unwrap(), None);
        store
            .set_invite_code_hash("$argon2id$override".to_owned())
            .await
            .unwrap();
    }
    let reopened = ChatStore::open(&temp.path).unwrap();
    assert_eq!(
        reopened.get_invite_code_hash().await.unwrap().as_deref(),
        Some("$argon2id$override")
    );
}

#[tokio::test]
async fn avatar_crud_persists_across_reopen() {
    let temp = TempDb::new();
    {
        let store = ChatStore::open(&temp.path).unwrap();
        assert_eq!(
            store.get_avatar("client-ana".to_owned()).await.unwrap(),
            None
        );
        store
            .upsert_avatar(
                "client-ana".to_owned(),
                vec![1, 2, 3, 4],
                "0123456789abcdef".to_owned(),
            )
            .await
            .unwrap();
        store
            .upsert_avatar(
                "client-ana".to_owned(),
                vec![5, 6, 7],
                "fedcba9876543210".to_owned(),
            )
            .await
            .unwrap();
    }
    let reopened = ChatStore::open(&temp.path).unwrap();
    assert_eq!(
        reopened.get_avatar("client-ana".to_owned()).await.unwrap(),
        Some((vec![5, 6, 7], "fedcba9876543210".to_owned()))
    );
    assert!(
        reopened
            .delete_avatar("client-ana".to_owned())
            .await
            .unwrap()
    );
    assert!(
        !reopened
            .delete_avatar("client-ana".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(
        reopened.get_avatar("client-ana".to_owned()).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn room_parents_stay_one_level_and_deleting_a_parent_frees_children() {
    let store = ChatStore::open_in_memory().unwrap();
    store
        .seed_rooms_if_empty_sync(&["geral".to_owned(), "jogos".to_owned()])
        .unwrap();
    let parent = |p: &str| RoomUpdate {
        parent: Some(Some(p.to_owned())),
        ..RoomUpdate::default()
    };

    assert_eq!(
        store
            .create_room("Filha".to_owned(), None, Some("nada".to_owned()))
            .await
            .unwrap(),
        CreateRoomOutcome::InvalidParent
    );
    let CreateRoomOutcome::Created(child) = store
        .create_room(
            "Filha".to_owned(),
            Some("hash".to_owned()),
            Some("geral".to_owned()),
        )
        .await
        .unwrap()
    else {
        panic!("child should be created");
    };
    assert!(child.locked);
    assert_eq!(child.parent.as_deref(), Some("geral"));

    for (id, target) in [("jogos", "filha"), ("geral", "jogos"), ("geral", "geral")] {
        assert_eq!(
            store
                .update_room(id.to_owned(), parent(target))
                .await
                .unwrap(),
            UpdateRoomOutcome::InvalidParent,
            "{id} -> {target}"
        );
    }

    let UpdateRoomOutcome::Updated(cleared) = store
        .update_room(
            "filha".to_owned(),
            RoomUpdate {
                password_hash: Some(None),
                ..RoomUpdate::default()
            },
        )
        .await
        .unwrap()
    else {
        panic!("room should be updated");
    };
    assert!(!cleared.locked);
    assert_eq!(cleared.parent.as_deref(), Some("geral"));

    assert_eq!(
        store.commit_delete_room("geral".to_owned()).await.unwrap(),
        DeleteRoomOutcome::Deleted
    );
    let records = store.room_records().await.unwrap();
    assert!(records.iter().all(|r| r.parent.is_none()));
}

#[tokio::test]
async fn migration_adds_room_columns_to_an_existing_database() {
    let temp = TempDb::new();
    {
        let conn = Connection::open(&temp.path).unwrap();
        conn.execute_batch(
            "CREATE TABLE rooms (id TEXT PRIMARY KEY, title TEXT NOT NULL, position INTEGER NOT NULL);
             INSERT INTO rooms (id, title, position) VALUES ('geral', 'Geral', 0);",
        )
        .unwrap();
    }
    let store = ChatStore::open(&temp.path).unwrap();
    let records = store.room_records().await.unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].parent.is_none() && records[0].password_hash.is_none());
    let updated = store
        .update_room(
            "geral".to_owned(),
            RoomUpdate {
                password_hash: Some(Some("hash".to_owned())),
                ..RoomUpdate::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(updated, UpdateRoomOutcome::Updated(room) if room.locked));
    drop(store);
    assert!(ChatStore::open(&temp.path).is_ok());
}
