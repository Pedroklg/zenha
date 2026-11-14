//! Armazenamento SQLite para mensagens de chat salvas por sala.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rooms_protocol::{CHAT_HISTORY_PAGE, ChatHistoryResponse, ChatMessage, SavedChatMessage};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FUTURE_SKEW_MS: u64 = 60_000;

#[derive(Debug, thiserror::Error)]
pub enum ChatStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("chat store lock poisoned")]
    LockPoisoned,
    #[error("blocking task failed: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    Created(ChatMessage),
    DuplicateBySameSender(ChatMessage),
    ConflictOtherSender,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    NotFound,
    Forbidden,
}

#[derive(Clone)]
pub struct ChatStore {
    conn: Arc<Mutex<Connection>>,
}

impl ChatStore {
    pub fn open(path: &Path) -> Result<Self, ChatStoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, ChatStoreError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, ChatStoreError> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chat_messages (
                room TEXT NOT NULL,
                id TEXT NOT NULL,
                sender_id TEXT NOT NULL,
                sender_name TEXT NOT NULL,
                text TEXT NOT NULL,
                sent_at_unix_ms INTEGER NOT NULL,
                saved_at_unix_ms INTEGER NOT NULL,
                PRIMARY KEY (room, id)
            );
            CREATE INDEX IF NOT EXISTS idx_chat_messages_room_order
                ON chat_messages (room, saved_at_unix_ms DESC, id DESC);",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn insert(
        &self,
        room: String,
        id: String,
        sender_id: String,
        sender_name: String,
        text: String,
        sent_at_unix_ms: u64,
    ) -> Result<InsertOutcome, ChatStoreError> {
        let saved_at_unix_ms = now_unix_ms();
        let sent_at_unix_ms = clamp_sent_at(sent_at_unix_ms, saved_at_unix_ms);
        self.with_conn(move |conn| {
            insert_sync(
                conn,
                NewMessage {
                    room: &room,
                    id: &id,
                    sender_id: &sender_id,
                    sender_name: &sender_name,
                    text: &text,
                    sent_at_unix_ms,
                    saved_at_unix_ms,
                },
            )
        })
        .await
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_at(
        &self,
        room: String,
        id: String,
        sender_id: String,
        sender_name: String,
        text: String,
        sent_at_unix_ms: u64,
        saved_at_unix_ms: u64,
    ) -> Result<InsertOutcome, ChatStoreError> {
        let sent_at_unix_ms = clamp_sent_at(sent_at_unix_ms, saved_at_unix_ms);
        self.with_conn(move |conn| {
            insert_sync(
                conn,
                NewMessage {
                    room: &room,
                    id: &id,
                    sender_id: &sender_id,
                    sender_name: &sender_name,
                    text: &text,
                    sent_at_unix_ms,
                    saved_at_unix_ms,
                },
            )
        })
        .await
    }

    pub async fn list_page(
        &self,
        room: String,
        before: Option<String>,
    ) -> Result<ChatHistoryResponse, ChatStoreError> {
        self.with_conn(move |conn| {
            list_page_sync(conn, &room, before.as_deref(), CHAT_HISTORY_PAGE)
        })
        .await
    }

    pub async fn delete(
        &self,
        room: String,
        id: String,
        sender_id: String,
    ) -> Result<DeleteOutcome, ChatStoreError> {
        self.with_conn(move |conn| delete_sync(conn, &room, &id, &sender_id))
            .await
    }

    async fn with_conn<T, F>(&self, op: F) -> Result<T, ChatStoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, ChatStoreError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| ChatStoreError::LockPoisoned)?;
            op(&mut guard)
        })
        .await?
    }
}

pub fn clamp_sent_at(sent_at_unix_ms: u64, server_now_ms: u64) -> u64 {
    sent_at_unix_ms.min(server_now_ms.saturating_add(MAX_FUTURE_SKEW_MS))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn to_sqlite_i64(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

fn from_sqlite_i64(ms: i64) -> u64 {
    u64::try_from(ms).unwrap_or(0)
}

struct NewMessage<'a> {
    room: &'a str,
    id: &'a str,
    sender_id: &'a str,
    sender_name: &'a str,
    text: &'a str,
    sent_at_unix_ms: u64,
    saved_at_unix_ms: u64,
}

fn insert_sync(
    conn: &mut Connection,
    msg: NewMessage<'_>,
) -> Result<InsertOutcome, ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "INSERT OR IGNORE INTO chat_messages
         (room, id, sender_id, sender_name, text, sent_at_unix_ms, saved_at_unix_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            msg.room,
            msg.id,
            msg.sender_id,
            msg.sender_name,
            msg.text,
            to_sqlite_i64(msg.sent_at_unix_ms),
            to_sqlite_i64(msg.saved_at_unix_ms)
        ],
    )?;

    if changed == 1 {
        tx.commit()?;
        return Ok(InsertOutcome::Created(ChatMessage {
            id: msg.id.to_owned(),
            sender: msg.sender_name.to_owned(),
            sent_at_unix_ms: msg.sent_at_unix_ms,
            text: msg.text.to_owned(),
            saved: true,
        }));
    }

    let existing: (String, String, String, i64) = tx.query_row(
        "SELECT sender_id, sender_name, text, sent_at_unix_ms
         FROM chat_messages
         WHERE room = ?1 AND id = ?2",
        params![msg.room, msg.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    tx.commit()?;

    if existing.0 == msg.sender_id {
        Ok(InsertOutcome::DuplicateBySameSender(ChatMessage {
            id: msg.id.to_owned(),
            sender: existing.1,
            sent_at_unix_ms: from_sqlite_i64(existing.3),
            text: existing.2,
            saved: true,
        }))
    } else {
        Ok(InsertOutcome::ConflictOtherSender)
    }
}

fn list_page_sync(
    conn: &mut Connection,
    room: &str,
    before: Option<&str>,
    limit: usize,
) -> Result<ChatHistoryResponse, ChatStoreError> {
    let tx = conn.transaction()?;
    let fetch_limit =
        to_sqlite_i64(u64::try_from(limit.max(1).saturating_add(1)).unwrap_or(u64::MAX));

    let mut messages = match before {
        None => {
            let mut stmt = tx.prepare(
                "SELECT id, sender_id, sender_name, text, sent_at_unix_ms
                 FROM chat_messages
                 WHERE room = ?1
                 ORDER BY saved_at_unix_ms DESC, id DESC
                 LIMIT ?2",
            )?;
            collect_rows(stmt.query_map(params![room, fetch_limit], map_saved_message)?)?
        }
        Some(before_id) => {
            let cursor: Option<i64> = tx
                .query_row(
                    "SELECT saved_at_unix_ms
                     FROM chat_messages
                     WHERE room = ?1 AND id = ?2",
                    params![room, before_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(cursor_saved_at) = cursor else {
                return Ok(ChatHistoryResponse {
                    messages: Vec::new(),
                    has_more: false,
                });
            };
            let mut stmt = tx.prepare(
                "SELECT id, sender_id, sender_name, text, sent_at_unix_ms
                 FROM chat_messages
                 WHERE room = ?1
                   AND saved_at_unix_ms <= ?2
                   AND (saved_at_unix_ms < ?2 OR id < ?3)
                 ORDER BY saved_at_unix_ms DESC, id DESC
                 LIMIT ?4",
            )?;
            collect_rows(stmt.query_map(
                params![room, cursor_saved_at, before_id, fetch_limit],
                map_saved_message,
            )?)?
        }
    };
    tx.commit()?;

    let limit = limit.max(1);
    let has_more = messages.len() > limit;
    if has_more {
        messages.truncate(limit);
    }
    messages.reverse();
    Ok(ChatHistoryResponse { messages, has_more })
}

fn delete_sync(
    conn: &mut Connection,
    room: &str,
    id: &str,
    sender_id: &str,
) -> Result<DeleteOutcome, ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let owner: Option<String> = tx
        .query_row(
            "SELECT sender_id FROM chat_messages WHERE room = ?1 AND id = ?2",
            params![room, id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(owner) = owner else {
        return Ok(DeleteOutcome::NotFound);
    };
    if owner != sender_id {
        return Ok(DeleteOutcome::Forbidden);
    }
    tx.execute(
        "DELETE FROM chat_messages WHERE room = ?1 AND id = ?2",
        params![room, id],
    )?;
    tx.commit()?;
    Ok(DeleteOutcome::Deleted)
}

fn map_saved_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedChatMessage> {
    let sent_i64: i64 = row.get(4)?;
    Ok(SavedChatMessage {
        message: ChatMessage {
            id: row.get(0)?,
            sender: row.get(2)?,
            sent_at_unix_ms: from_sqlite_i64(sent_i64),
            text: row.get(3)?,
            saved: true,
        },
        sender_id: row.get(1)?,
    })
}

fn collect_rows<I>(rows: I) -> Result<Vec<SavedChatMessage>, ChatStoreError>
where
    I: Iterator<Item = rusqlite::Result<SavedChatMessage>>,
{
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

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
}
