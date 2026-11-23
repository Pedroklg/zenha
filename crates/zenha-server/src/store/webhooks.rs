//! Incoming webhooks: creation under a quota, lookup by id, removal and the posts kept for 12 hours.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use zenha_protocol::{
    AdminWebhook, MAX_HISTORY_WEBHOOK_POSTS, MAX_WEBHOOKS_COUNT, WEBHOOK_POST_TTL_MS, WebhookPost,
};

use super::messages::now_unix_ms;
use super::{ChatStore, ChatStoreError};

/// Window during which duplicate links in the same room are ignored for personal webhooks.
pub const PERSONAL_WEBHOOK_DEDUP_WINDOW_MS: u64 = 10 * 60 * 1000;

/// Stored webhook; `token_hash` is the hex SHA-256 of the secret, never the secret itself.
pub struct WebhookRecord {
    pub room: String,
    pub name: String,
    pub token_hash: String,
}

/// Hex SHA-256 of a webhook token, the only form in which it is stored.
pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
            let tx = conn.transaction()?;
            let removed = tx.execute("DELETE FROM webhooks WHERE id = ?1", params![id])?;
            tx.execute(
                "DELETE FROM webhook_posts WHERE webhook_id = ?1",
                params![id],
            )?;
            tx.commit()?;
            Ok(removed > 0)
        })
        .await
    }

    /// Keeps a delivered post for the room history; `sent_at_unix_ms` is its storage time.
    /// Returns `false`, storing nothing, when its first link repeats the last post of the same webhook.
    pub async fn save_webhook_post(
        &self,
        room: String,
        webhook_id: String,
        post: WebhookPost,
        post_json: String,
    ) -> Result<bool, ChatStoreError> {
        self.with_conn(move |conn| {
            let cutoff = post.sent_at_unix_ms.saturating_sub(WEBHOOK_POST_TTL_MS);
            conn.execute(
                "DELETE FROM webhook_posts WHERE created_at_unix_ms < ?1",
                params![to_sql_ms(cutoff)],
            )?;
            if let Some(url) = post.embeds.first().and_then(|embed| embed.url.as_deref()) {
                let last_url: Option<Option<String>> = conn
                    .query_row(
                        "SELECT json_extract(post_json, '$.embeds[0].url') FROM webhook_posts
                         WHERE webhook_id = ?1 ORDER BY created_at_unix_ms DESC, rowid DESC LIMIT 1",
                        params![webhook_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if last_url.flatten().as_deref() == Some(url) {
                    return Ok(false);
                }
            }
            conn.execute(
                "INSERT OR IGNORE INTO webhook_posts (id, room, webhook_id, post_json, created_at_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    post.id,
                    room,
                    webhook_id,
                    post_json,
                    to_sql_ms(post.sent_at_unix_ms)
                ],
            )?;
            Ok(true)
        })
        .await
    }

    /// Keeps a delivered personal-hook post for the room history.
    /// Returns `false`, storing nothing, when any sender already posted the same link to `room`
    /// within the last 10 minutes.
    pub async fn save_personal_webhook_post(
        &self,
        room: String,
        webhook_id: String,
        post: WebhookPost,
        post_json: String,
    ) -> Result<bool, ChatStoreError> {
        self.with_conn(move |conn| {
            let cutoff = post.sent_at_unix_ms.saturating_sub(WEBHOOK_POST_TTL_MS);
            conn.execute(
                "DELETE FROM webhook_posts WHERE created_at_unix_ms < ?1",
                params![to_sql_ms(cutoff)],
            )?;
            if let Some(key) = webhook_post_dedup_key(&post) {
                let window_start = post
                    .sent_at_unix_ms
                    .saturating_sub(PERSONAL_WEBHOOK_DEDUP_WINDOW_MS);
                let mut stmt = conn.prepare(
                    "SELECT post_json FROM webhook_posts
                     WHERE room = ?1 AND created_at_unix_ms > ?2",
                )?;
                let rows = stmt.query_map(params![room, to_sql_ms(window_start)], |row| {
                    row.get::<_, String>(0)
                })?;
                for raw in rows {
                    if let Ok(existing) = serde_json::from_str::<WebhookPost>(&raw?)
                        && webhook_post_dedup_key(&existing).as_deref() == Some(key.as_str())
                    {
                        return Ok(false);
                    }
                }
            }
            conn.execute(
                "INSERT OR IGNORE INTO webhook_posts (id, room, webhook_id, post_json, created_at_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    post.id,
                    room,
                    webhook_id,
                    post_json,
                    to_sql_ms(post.sent_at_unix_ms)
                ],
            )?;
            Ok(true)
        })
        .await
    }

    /// Posts of the room younger than 12 hours at `now_unix_ms`, oldest first.
    pub async fn list_webhook_posts(
        &self,
        room: String,
        now_unix_ms: u64,
    ) -> Result<Vec<WebhookPost>, ChatStoreError> {
        self.with_conn(move |conn| {
            let cutoff = now_unix_ms.saturating_sub(WEBHOOK_POST_TTL_MS);
            let mut stmt = conn.prepare(
                "SELECT post_json FROM webhook_posts
                 WHERE room = ?1 AND created_at_unix_ms >= ?2
                 ORDER BY created_at_unix_ms DESC, rowid DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                params![
                    room,
                    to_sql_ms(cutoff),
                    i64::try_from(MAX_HISTORY_WEBHOOK_POSTS).unwrap_or(i64::MAX)
                ],
                |row| row.get::<_, String>(0),
            )?;
            let mut posts = Vec::new();
            for json in rows {
                if let Ok(post) = serde_json::from_str::<WebhookPost>(&json?) {
                    posts.push(post);
                }
            }
            posts.reverse();
            Ok(posts)
        })
        .await
    }
}

fn to_sql_ms(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
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
    conn.execute("DELETE FROM webhook_posts WHERE room = ?1", params![room])?;
    Ok(())
}

/// Key used to suppress duplicate posts sent to the same room:
/// `embeds[0].url` when present, else a Gamers Club match URL inside an embed field.
pub fn webhook_post_dedup_key(post: &WebhookPost) -> Option<String> {
    let embed = post.embeds.first()?;
    if let Some(url) = &embed.url {
        return Some(url.clone());
    }
    embed
        .fields
        .iter()
        .find_map(|field| extract_gc_match_url(&field.value))
}

fn extract_gc_match_url(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(at) = rest.find("https://") {
        let candidate = &rest[at..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\''))
            .unwrap_or(candidate.len());
        let url = &candidate[..end];
        if let Some(after_scheme) = url.strip_prefix("https://")
            && let Some((host, path)) = after_scheme.split_once('/')
            && is_gamersclub_host(host)
            && path
                .strip_prefix("lobby/partida/")
                .is_some_and(|id| !id.is_empty())
        {
            return Some(url.to_owned());
        }
        rest = &candidate["https://".len()..];
    }
    None
}

fn is_gamersclub_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower == "gamersclub.com.br"
        || lower.ends_with(".gamersclub.com.br")
        || lower == "gamersclub.gg"
        || lower.ends_with(".gamersclub.gg")
}
