//! SQLite schema and migrations.

use rusqlite::Connection;

use super::{BUSY_TIMEOUT, ChatStoreError};

pub(super) fn init_schema(conn: &Connection) -> Result<(), ChatStoreError> {
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
            );
            CREATE TABLE IF NOT EXISTS avatars (
                identity TEXT PRIMARY KEY,
                png BLOB NOT NULL,
                version TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS webhooks (
                id TEXT PRIMARY KEY,
                room TEXT NOT NULL,
                name TEXT NOT NULL,
                token_hash TEXT NOT NULL,
                created_at_unix_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS webhook_posts (
                id TEXT PRIMARY KEY,
                room TEXT NOT NULL,
                webhook_id TEXT NOT NULL,
                post_json TEXT NOT NULL,
                created_at_unix_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_webhook_posts_room_order
                ON webhook_posts (room, created_at_unix_ms DESC);
            CREATE TABLE IF NOT EXISTS client_reports (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id TEXT NOT NULL,
                display_name TEXT NOT NULL,
                version TEXT NOT NULL,
                os TEXT NOT NULL,
                received_at_unix_ms INTEGER NOT NULL,
                events_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_client_reports_client
                ON client_reports (client_id, id DESC);",
    )?;
    migrate_rooms(conn)?;
    Ok(())
}

fn migrate_rooms(conn: &Connection) -> Result<(), ChatStoreError> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('rooms')")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for (column, ddl) in [
        (
            "password_hash",
            "ALTER TABLE rooms ADD COLUMN password_hash TEXT",
        ),
        ("parent", "ALTER TABLE rooms ADD COLUMN parent TEXT"),
    ] {
        if !columns.iter().any(|c| c == column) {
            conn.execute(ddl, [])?;
        }
    }
    Ok(())
}
