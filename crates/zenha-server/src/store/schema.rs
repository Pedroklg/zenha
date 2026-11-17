//! Esquema do SQLite e migrações.

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
            );",
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
