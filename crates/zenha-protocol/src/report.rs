//! Error reports the client sends to `POST /client-reports` when something fails.

use serde::{Deserialize, Serialize};

use crate::{MAX_CLIENT_ID_LEN, MAX_DISPLAY_NAME_LEN};

pub const MAX_CLIENT_REPORT_BODY_BYTES: usize = 64 * 1024;
pub const MAX_REPORT_EVENTS: usize = 50;
pub const MAX_REPORT_MESSAGE_LEN: usize = 1000;
pub const MAX_REPORT_TARGET_LEN: usize = 128;
pub const MAX_REPORT_META_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportLevel {
    Warn,
    Error,
    /// The previous run ended in a panic or a native crash.
    Crash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEvent {
    pub at_unix_ms: u64,
    pub level: ReportLevel,
    pub target: String,
    pub message: String,
    /// How many times the same event happened since the first one in this report.
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientReport {
    pub client_id: String,
    pub display_name: String,
    pub version: String,
    pub os: String,
    pub events: Vec<ReportEvent>,
}

impl ClientReport {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.client_id.trim().is_empty() || self.client_id.len() > MAX_CLIENT_ID_LEN {
            return Err("invalid client id");
        }
        if self.display_name.chars().count() > MAX_DISPLAY_NAME_LEN {
            return Err("display name too long");
        }
        if self.version.len() > MAX_REPORT_META_LEN || self.os.len() > MAX_REPORT_META_LEN {
            return Err("version or os too long");
        }
        if self.events.is_empty() || self.events.len() > MAX_REPORT_EVENTS {
            return Err("invalid number of events");
        }
        let oversized = self.events.iter().any(|event| {
            event.target.len() > MAX_REPORT_TARGET_LEN
                || event.message.chars().count() > MAX_REPORT_MESSAGE_LEN
        });
        if oversized {
            return Err("event too long");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(events: Vec<ReportEvent>) -> ClientReport {
        ClientReport {
            client_id: "abc".to_owned(),
            display_name: "Ana".to_owned(),
            version: "0.1.19".to_owned(),
            os: "Windows 10.0.26300".to_owned(),
            events,
        }
    }

    fn event(message: &str) -> ReportEvent {
        ReportEvent {
            at_unix_ms: 1,
            level: ReportLevel::Warn,
            target: "zenha_core::audio".to_owned(),
            message: message.to_owned(),
            count: 1,
        }
    }

    #[test]
    fn validate_checks_ids_counts_and_lengths() {
        assert_eq!(report(vec![event("ok")]).validate(), Ok(()));
        assert!(report(Vec::new()).validate().is_err());
        assert!(
            report(vec![event("x"); MAX_REPORT_EVENTS + 1])
                .validate()
                .is_err()
        );
        assert!(
            report(vec![event(&"é".repeat(MAX_REPORT_MESSAGE_LEN + 1))])
                .validate()
                .is_err()
        );
        let mut blank = report(vec![event("ok")]);
        blank.client_id = " ".to_owned();
        assert!(blank.validate().is_err());
    }

    #[test]
    fn report_round_trips_through_json_with_lowercase_levels() {
        let mut crash = event("previous run crashed");
        crash.level = ReportLevel::Crash;
        let original = report(vec![event("ok"), crash]);
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("\"level\":\"warn\""));
        assert!(json.contains("\"level\":\"crash\""));
        assert_eq!(
            serde_json::from_str::<ClientReport>(&json).unwrap(),
            original
        );
    }
}
