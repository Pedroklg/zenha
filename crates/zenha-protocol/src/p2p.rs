//! Protocolo de sinalização P2P trocado pelo canal de dados da sala LiveKit.

use serde::{Deserialize, Serialize};

pub const P2P_SIGNAL_TOPIC: &str = "zenha.p2p.v1";
pub const MAX_P2P_SIGNAL_BYTES: usize = 16_384;
pub const MAX_P2P_SESSION_ID_LEN: usize = 64;
pub const MAX_P2P_REASON_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum P2pSignal {
    Request {
        share_epoch: u64,
        session_id: String,
    },
    Reject {
        session_id: String,
        reason: String,
    },
    Offer {
        share_epoch: u64,
        session_id: String,
        sdp: String,
    },
    Answer {
        session_id: String,
        sdp: String,
    },
    Ice {
        session_id: String,
        candidate: String,
        sdp_mid: String,
        sdp_mline_index: i32,
    },
    Fallback {
        session_id: String,
        reason: String,
    },
    Bye {
        session_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum P2pSignalError {
    #[error("p2p signal payload is empty")]
    Empty,
    #[error("p2p signal payload exceeds {max} bytes ({actual} bytes)")]
    TooLarge { actual: usize, max: usize },
    #[error("invalid p2p signal field: {0}")]
    InvalidField(&'static str),
    #[error("invalid p2p signal json: {0}")]
    InvalidJson(String),
}

impl P2pSignal {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Request { session_id, .. }
            | Self::Reject { session_id, .. }
            | Self::Offer { session_id, .. }
            | Self::Answer { session_id, .. }
            | Self::Ice { session_id, .. }
            | Self::Fallback { session_id, .. }
            | Self::Bye { session_id } => session_id,
        }
    }

    pub fn validate(&self) -> Result<(), P2pSignalError> {
        validate_session_id(self.session_id())?;
        match self {
            Self::Request { .. } | Self::Bye { .. } => Ok(()),
            Self::Reject { reason, .. } | Self::Fallback { reason, .. } => validate_reason(reason),
            Self::Offer { sdp, .. } | Self::Answer { sdp, .. } => {
                if sdp.trim().is_empty() {
                    return Err(P2pSignalError::InvalidField("sdp"));
                }
                Ok(())
            }
            Self::Ice {
                candidate,
                sdp_mid,
                sdp_mline_index,
                ..
            } => {
                if candidate.trim().is_empty() {
                    return Err(P2pSignalError::InvalidField("candidate"));
                }
                if sdp_mid.chars().any(char::is_control) {
                    return Err(P2pSignalError::InvalidField("sdp_mid"));
                }
                if *sdp_mline_index < 0 {
                    return Err(P2pSignalError::InvalidField("sdp_mline_index"));
                }
                Ok(())
            }
        }
    }
}

fn validate_session_id(session_id: &str) -> Result<(), P2pSignalError> {
    let trimmed = session_id.trim();
    if trimmed.is_empty()
        || trimmed.len() != session_id.len()
        || session_id.len() > MAX_P2P_SESSION_ID_LEN
        || session_id.chars().any(char::is_control)
    {
        return Err(P2pSignalError::InvalidField("session_id"));
    }
    Ok(())
}

fn validate_reason(reason: &str) -> Result<(), P2pSignalError> {
    if reason.chars().count() > MAX_P2P_REASON_LEN || reason.chars().any(char::is_control) {
        return Err(P2pSignalError::InvalidField("reason"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_round_trips_through_json_and_validates() {
        let signal = P2pSignal::Ice {
            session_id: "sess-01".to_owned(),
            candidate: "candidate:1 1 udp 2122260223 192.168.1.2 50000 typ host".to_owned(),
            sdp_mid: "0".to_owned(),
            sdp_mline_index: 0,
        };
        let json = serde_json::to_string(&signal).unwrap();
        assert!(json.contains(r#""type":"ice""#));
        let parsed: P2pSignal = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, signal);
        assert_eq!(parsed.validate(), Ok(()));
        assert_eq!(
            P2pSignal::Bye {
                session_id: " s".to_owned()
            }
            .validate(),
            Err(P2pSignalError::InvalidField("session_id"))
        );
    }
}
