//! Error reports sent by clients, kept for 7 days and capped per client.

use rusqlite::params;
use zenha_protocol::ClientReport;

use super::{ChatStore, ChatStoreError};

pub const CLIENT_REPORT_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub const MAX_REPORTS_PER_CLIENT: u32 = 200;

impl ChatStore {
    pub async fn save_client_report(
        &self,
        report: ClientReport,
        events_json: String,
        now_unix_ms: u64,
    ) -> Result<(), ChatStoreError> {
        self.with_conn(move |conn| {
            let cutoff = now_unix_ms.saturating_sub(CLIENT_REPORT_TTL_MS);
            conn.execute(
                "DELETE FROM client_reports WHERE received_at_unix_ms < ?1",
                params![to_sql_ms(cutoff)],
            )?;
            conn.execute(
                "INSERT INTO client_reports (client_id, display_name, version, os, received_at_unix_ms, events_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    report.client_id,
                    report.display_name,
                    report.version,
                    report.os,
                    to_sql_ms(now_unix_ms),
                    events_json
                ],
            )?;
            conn.execute(
                "DELETE FROM client_reports WHERE client_id = ?1 AND id NOT IN (
                     SELECT id FROM client_reports WHERE client_id = ?1 ORDER BY id DESC LIMIT ?2
                 )",
                params![report.client_id, MAX_REPORTS_PER_CLIENT],
            )?;
            Ok(())
        })
        .await
    }
}

fn to_sql_ms(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use zenha_protocol::{ReportEvent, ReportLevel};

    use super::*;

    fn report(client_id: &str) -> ClientReport {
        ClientReport {
            client_id: client_id.to_owned(),
            display_name: "Ana".to_owned(),
            version: "0.1.19".to_owned(),
            os: "Windows".to_owned(),
            events: vec![ReportEvent {
                at_unix_ms: 1,
                level: ReportLevel::Warn,
                target: "zenha_core".to_owned(),
                message: "audio device failed".to_owned(),
                count: 1,
            }],
        }
    }

    async fn save(store: &ChatStore, client_id: &str, at: u64) {
        store
            .save_client_report(report(client_id), "[]".to_owned(), at)
            .await
            .unwrap();
    }

    async fn count(store: &ChatStore, client_id: &str) -> i64 {
        let client_id = client_id.to_owned();
        store
            .with_conn(move |conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM client_reports WHERE client_id = ?1",
                    params![client_id],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reports_expire_after_the_ttl() {
        let store = ChatStore::open_in_memory().unwrap();
        save(&store, "a", 1_000).await;
        save(&store, "b", 2_000).await;
        save(&store, "b", 1_500 + CLIENT_REPORT_TTL_MS).await;
        assert_eq!(count(&store, "a").await, 0);
        assert_eq!(count(&store, "b").await, 2);
    }

    #[tokio::test]
    async fn reports_are_capped_per_client() {
        let store = ChatStore::open_in_memory().unwrap();
        for i in 0..(MAX_REPORTS_PER_CLIENT + 3) {
            save(&store, "a", 1_000 + u64::from(i)).await;
        }
        save(&store, "b", 5_000).await;
        assert_eq!(count(&store, "a").await, i64::from(MAX_REPORTS_PER_CLIENT));
        assert_eq!(count(&store, "b").await, 1);
    }
}
