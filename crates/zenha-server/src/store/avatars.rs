//! Avatares dos participantes guardados como PNG no SQLite.

use rusqlite::{OptionalExtension, params};

use super::{ChatStore, ChatStoreError};

impl ChatStore {
    pub async fn upsert_avatar(
        &self,
        identity: String,
        png: Vec<u8>,
        version: String,
    ) -> Result<(), ChatStoreError> {
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO avatars (identity, png, version) VALUES (?1, ?2, ?3)
                 ON CONFLICT(identity) DO UPDATE SET png = excluded.png, version = excluded.version",
                params![identity, png, version],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn get_avatar(
        &self,
        identity: String,
    ) -> Result<Option<(Vec<u8>, String)>, ChatStoreError> {
        self.with_conn(move |conn| {
            let row = conn
                .query_row(
                    "SELECT png, version FROM avatars WHERE identity = ?1",
                    params![identity],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            Ok(row)
        })
        .await
    }

    pub async fn delete_avatar(&self, identity: String) -> Result<bool, ChatStoreError> {
        self.with_conn(move |conn| {
            let deleted =
                conn.execute("DELETE FROM avatars WHERE identity = ?1", params![identity])?;
            Ok(deleted > 0)
        })
        .await
    }
}
