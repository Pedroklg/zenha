//! Incoming webhooks: creation under a quota, lookup by id and removal.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use zenha_protocol::{AdminWebhook, MAX_WEBHOOKS_COUNT};

use super::messages::now_unix_ms;
use super::{ChatStore, ChatStoreError};

/// Stored webhook; `token_hash` is the hex SHA-256 of the secret, never the secret itself.
pub struct WebhookRecord {
    pub room: String,
    pub name: String,
    pub token_hash: String,
}

/// Hex SHA-256 of a webhook token, the only form in which it is stored.
pub fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

impl ChatStore {
    /// Returns `None` when the webhook quota is already used.
    pub async fn create_webhook(
        &self,
        id: String,
        room: String,
        name: String,
        token_hash: String,
    ) -> Result<Option<AdminWebhook>, ChatStoreError> {
        self.with_conn(move |conn| create_webhook_sync(conn, id, room, name, &token_hash))
            .await
    }

    pub async fn list_webhooks(&self) -> Result<Vec<AdminWebhook>, ChatStoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, room, name, created_at_unix_ms FROM webhooks
                 ORDER BY created_at_unix_ms ASC, id ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AdminWebhook {
                    id: row.get(0)?,
                    room: row.get(1)?,
                    name: row.get(2)?,
                    created_at_unix_ms: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn get_webhook(&self, id: String) -> Result<Option<WebhookRecord>, ChatStoreError> {
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT room, name, token_hash FROM webhooks WHERE id = ?1",
                    params![id],
                    |row| {
                        Ok(WebhookRecord {
                            room: row.get(0)?,
                            name: row.get(1)?,
                            token_hash: row.get(2)?,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    pub async fn delete_webhook(&self, id: String) -> Result<bool, ChatStoreError> {
        self.with_conn(move |conn| {
            let removed = conn.execute("DELETE FROM webhooks WHERE id = ?1", params![id])?;
            Ok(removed > 0)
        })
        .await
    }
}

fn create_webhook_sync(
    conn: &mut Connection,
    id: String,
    room: String,
    name: String,
    token_hash: &str,
) -> Result<Option<AdminWebhook>, ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM webhooks", [], |row| row.get(0))?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_WEBHOOKS_COUNT {
        return Ok(None);
    }
    let created_at_unix_ms = now_unix_ms();
    tx.execute(
        "INSERT INTO webhooks (id, room, name, token_hash, created_at_unix_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id,
            room,
            name,
            token_hash,
            i64::try_from(created_at_unix_ms).unwrap_or(i64::MAX)
        ],
    )?;
    tx.commit()?;
    Ok(Some(AdminWebhook {
        id,
        room,
        name,
        created_at_unix_ms,
    }))
}

pub(super) fn delete_room_webhooks(conn: &Connection, room: &str) -> Result<(), ChatStoreError> {
    conn.execute("DELETE FROM webhooks WHERE room = ?1", params![room])?;
    Ok(())
}
