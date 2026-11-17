//! SQLite storage for saved chat messages, rooms and settings.

mod avatars;
mod messages;
mod rooms;
mod schema;
mod webhooks;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

pub use self::messages::{DeleteOutcome, InsertOutcome, now_unix_ms};
pub use self::rooms::{
    CreateRoomOutcome, DeleteRoomCheck, DeleteRoomOutcome, RoomRecord, RoomUpdate,
    UpdateRoomOutcome,
};
pub use self::webhooks::token_hash;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
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
        schema::init_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
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
