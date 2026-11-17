//! Saved chat messages: idempotent insertion, pagination and removal.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use zenha_protocol::{CHAT_HISTORY_PAGE, ChatHistoryResponse, ChatMessage, SavedChatMessage};

use super::{ChatStore, ChatStoreError};

const MAX_FUTURE_SKEW_MS: u64 = 60_000;

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

impl ChatStore {
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
}

pub fn clamp_sent_at(sent_at_unix_ms: u64, server_now_ms: u64) -> u64 {
    sent_at_unix_ms.min(server_now_ms.saturating_add(MAX_FUTURE_SKEW_MS))
}

pub fn now_unix_ms() -> u64 {
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
