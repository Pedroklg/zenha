//! Armazenamento SQLite para mensagens de chat salvas, salas e configurações.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rooms_protocol::{
    AdminRoom, CHAT_HISTORY_PAGE, ChatHistoryResponse, ChatMessage, MAX_ROOM_NAME_LEN,
    MAX_ROOMS_COUNT, SavedChatMessage,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FUTURE_SKEW_MS: u64 = 60_000;
const INVITE_HASH_SETTING_KEY: &str = "invite_code_hash";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateRoomOutcome {
    Created(AdminRoom),
    LimitReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteRoomCheck {
    Ready,
    NotFound,
    LastRoom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteRoomOutcome {
    Deleted,
    NotFound,
    LastRoom,
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
                ON chat_messages (room, saved_at_unix_ms DESC, id DESC);
            CREATE TABLE IF NOT EXISTS rooms (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                position INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_rooms_position
                ON rooms (position ASC, id ASC);
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn seed_rooms_if_empty_sync(&self, rooms: &[String]) -> Result<(), ChatStoreError> {
        let mut guard = self.conn.lock().map_err(|_| ChatStoreError::LockPoisoned)?;
        seed_rooms_sync(&mut guard, rooms)
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
    #[expect(
        clippy::too_many_arguments,
        reason = "test helper populates every message column explicitly"
    )]
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

    pub async fn delete_as_admin(&self, room: String, id: String) -> Result<bool, ChatStoreError> {
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "DELETE FROM chat_messages WHERE room = ?1 AND id = ?2",
                params![room, id],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    pub async fn list_rooms(&self) -> Result<Vec<AdminRoom>, ChatStoreError> {
        self.with_conn(list_rooms_sync).await
    }

    pub async fn room_exists(&self, id: String) -> Result<bool, ChatStoreError> {
        self.with_conn(move |conn| {
            let found: Option<i64> = conn
                .query_row("SELECT 1 FROM rooms WHERE id = ?1", params![id], |row| {
                    row.get(0)
                })
                .optional()?;
            Ok(found.is_some())
        })
        .await
    }

    pub async fn create_room(&self, title: String) -> Result<CreateRoomOutcome, ChatStoreError> {
        self.with_conn(move |conn| create_room_sync(conn, &title))
            .await
    }

    pub async fn update_room(
        &self,
        id: String,
        title: Option<String>,
        position: Option<usize>,
    ) -> Result<Option<AdminRoom>, ChatStoreError> {
        self.with_conn(move |conn| update_room_sync(conn, &id, title.as_deref(), position))
            .await
    }

    pub async fn check_can_delete_room(
        &self,
        id: String,
    ) -> Result<DeleteRoomCheck, ChatStoreError> {
        self.with_conn(move |conn| check_can_delete_room_sync(conn, &id))
            .await
    }

    pub async fn commit_delete_room(
        &self,
        id: String,
    ) -> Result<DeleteRoomOutcome, ChatStoreError> {
        self.with_conn(move |conn| commit_delete_room_sync(conn, &id))
            .await
    }

    pub async fn get_invite_code_hash(&self) -> Result<Option<String>, ChatStoreError> {
        self.with_conn(|conn| {
            let value = conn
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    params![INVITE_HASH_SETTING_KEY],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(value)
        })
        .await
    }

    pub async fn set_invite_code_hash(&self, hash: String) -> Result<(), ChatStoreError> {
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![INVITE_HASH_SETTING_KEY, hash],
            )?;
            Ok(())
        })
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

fn seed_rooms_sync(conn: &mut Connection, rooms: &[String]) -> Result<(), ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM rooms", [], |row| row.get(0))?;
    if count == 0 {
        for (idx, id) in rooms.iter().enumerate() {
            let pos = i64::try_from(idx).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO rooms (id, title, position) VALUES (?1, ?2, ?3)",
                params![id, id, pos],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn list_rooms_sync(conn: &mut Connection) -> Result<Vec<AdminRoom>, ChatStoreError> {
    let mut stmt =
        conn.prepare("SELECT id, title, position FROM rooms ORDER BY position ASC, id ASC")?;
    let rows = stmt.query_map([], |row| {
        let pos: i64 = row.get(2)?;
        Ok(AdminRoom {
            id: row.get(0)?,
            title: row.get(1)?,
            position: usize::try_from(pos).unwrap_or(0),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn create_room_sync(
    conn: &mut Connection,
    title: &str,
) -> Result<CreateRoomOutcome, ChatStoreError> {
    let trimmed = title.trim();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = load_ordered_rooms(&tx)?;
    if existing.len() >= MAX_ROOMS_COUNT {
        return Ok(CreateRoomOutcome::LimitReached);
    }
    let existing_ids: Vec<String> = existing.iter().map(|(id, _)| id.clone()).collect();
    let id = unique_room_id(trimmed, &existing_ids);
    let position = existing.len();
    let pos_i64 = i64::try_from(position).unwrap_or(i64::MAX);
    tx.execute(
        "INSERT INTO rooms (id, title, position) VALUES (?1, ?2, ?3)",
        params![id, trimmed, pos_i64],
    )?;
    tx.commit()?;
    Ok(CreateRoomOutcome::Created(AdminRoom {
        id,
        title: trimmed.to_owned(),
        position,
    }))
}

fn update_room_sync(
    conn: &mut Connection,
    id: &str,
    title: Option<&str>,
    position: Option<usize>,
) -> Result<Option<AdminRoom>, ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut rooms = load_ordered_rooms(&tx)?;
    let Some(mut idx) = rooms.iter().position(|(room_id, _)| room_id == id) else {
        return Ok(None);
    };
    if let Some(new_title) = title {
        rooms[idx].1 = new_title.trim().to_owned();
    }
    if let Some(target) = position {
        let clamped = target.min(rooms.len().saturating_sub(1));
        if clamped != idx {
            let item = rooms.remove(idx);
            rooms.insert(clamped, item);
            idx = clamped;
        }
    }
    for (pos, (room_id, room_title)) in rooms.iter().enumerate() {
        let pos_i64 = i64::try_from(pos).unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE rooms SET title = ?1, position = ?2 WHERE id = ?3",
            params![room_title, pos_i64, room_id],
        )?;
    }
    tx.commit()?;
    Ok(Some(AdminRoom {
        id: rooms[idx].0.clone(),
        title: rooms[idx].1.clone(),
        position: idx,
    }))
}

fn check_can_delete_room_sync(
    conn: &mut Connection,
    id: &str,
) -> Result<DeleteRoomCheck, ChatStoreError> {
    let tx = conn.transaction()?;
    let rooms = load_ordered_rooms(&tx)?;
    if !rooms.iter().any(|(room_id, _)| room_id == id) {
        return Ok(DeleteRoomCheck::NotFound);
    }
    if rooms.len() <= 1 {
        return Ok(DeleteRoomCheck::LastRoom);
    }
    Ok(DeleteRoomCheck::Ready)
}

fn commit_delete_room_sync(
    conn: &mut Connection,
    id: &str,
) -> Result<DeleteRoomOutcome, ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let rooms = load_ordered_rooms(&tx)?;
    if !rooms.iter().any(|(room_id, _)| room_id == id) {
        return Ok(DeleteRoomOutcome::NotFound);
    }
    if rooms.len() <= 1 {
        return Ok(DeleteRoomOutcome::LastRoom);
    }
    tx.execute("DELETE FROM rooms WHERE id = ?1", params![id])?;
    tx.execute("DELETE FROM chat_messages WHERE room = ?1", params![id])?;
    let remaining = rooms.into_iter().filter(|(room_id, _)| room_id != id);
    for (pos, (room_id, _)) in remaining.enumerate() {
        let pos_i64 = i64::try_from(pos).unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE rooms SET position = ?1 WHERE id = ?2",
            params![pos_i64, room_id],
        )?;
    }
    tx.commit()?;
    Ok(DeleteRoomOutcome::Deleted)
}

fn load_ordered_rooms(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Vec<(String, String)>, ChatStoreError> {
    let mut stmt = tx.prepare("SELECT id, title FROM rooms ORDER BY position ASC, id ASC")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn fold_latin(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' | 'Á' | 'À' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'ø' | 'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => 'o',
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
        'ç' | 'Ç' => 'c',
        'ñ' | 'Ñ' => 'n',
        'ý' | 'ÿ' | 'Ý' => 'y',
        other => other.to_ascii_lowercase(),
    }
}

pub fn slugify_room_title(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in title.trim().chars() {
        let mapped = fold_latin(ch);
        if mapped.is_ascii_lowercase() || mapped.is_ascii_digit() || mapped == '_' {
            out.push(mapped);
            last_dash = false;
        } else if !out.is_empty() && !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > MAX_ROOM_NAME_LEN {
        out.truncate(MAX_ROOM_NAME_LEN);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "sala".to_owned()
    } else {
        out
    }
}

fn unique_room_id(title: &str, existing: &[String]) -> String {
    let base = slugify_room_title(title);
    if !existing.iter().any(|id| id == &base) {
        return base;
    }
    for n in 2..=(MAX_ROOMS_COUNT + 2) {
        let suffix = format!("-{n}");
        let max_prefix = MAX_ROOM_NAME_LEN.saturating_sub(suffix.len());
        let mut prefix: String = base.chars().take(max_prefix).collect();
        while prefix.ends_with('-') {
            prefix.pop();
        }
        let candidate = if prefix.is_empty() {
            format!("sala{suffix}")
        } else {
            format!("{prefix}{suffix}")
        };
        if !existing.iter().any(|id| id == &candidate) {
            return candidate;
        }
    }
    base
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
                },
                AdminRoom {
                    id: "jogos".to_owned(),
                    title: "jogos".to_owned(),
                    position: 1,
                },
                AdminRoom {
                    id: "afk".to_owned(),
                    title: "afk".to_owned(),
                    position: 2,
                },
            ]
        );

        store
            .update_room("geral".to_owned(), Some("Sala Geral".to_owned()), None)
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
        use rooms_protocol::validate_room_name;

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

        let created = store.create_room("Música & Voz".to_owned()).await.unwrap();
        assert_eq!(
            created,
            CreateRoomOutcome::Created(AdminRoom {
                id: "musica-voz".to_owned(),
                title: "Música & Voz".to_owned(),
                position: 2,
            })
        );

        let moved = store
            .update_room("musica-voz".to_owned(), Some("Música".to_owned()), Some(0))
            .await
            .unwrap()
            .unwrap();
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
            let outcome = store.create_room(format!("Sala {i}")).await.unwrap();
            assert!(matches!(outcome, CreateRoomOutcome::Created(_)));
        }
        assert_eq!(
            store.create_room("Extra".to_owned()).await.unwrap(),
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
}
