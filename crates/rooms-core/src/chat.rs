use rooms_protocol::{ChatMessage, MAX_CHAT_MESSAGE_LEN, validate_chat_text};

use crate::api::SessionError;

pub const CHAT_TOPIC: &str = "chat";
const MAX_PAYLOAD_BYTES: usize = MAX_CHAT_MESSAGE_LEN * 4 + 512;
const MAX_ID_LEN: usize = 64;

pub fn build_outgoing(
    id: String,
    sender: &str,
    text: &str,
    sent_at_unix_ms: u64,
) -> Result<(ChatMessage, Vec<u8>), SessionError> {
    validate_chat_text(text).map_err(|err| SessionError::InvalidInput(err.to_string()))?;
    let message = ChatMessage {
        id,
        sender: sender.to_owned(),
        sent_at_unix_ms,
        text: text.to_owned(),
    };
    let payload =
        serde_json::to_vec(&message).map_err(|err| SessionError::Internal(err.to_string()))?;
    Ok((message, payload))
}

/// The sender is always the identity reported by the backend, never the payload's own field.
pub fn parse_incoming(sender_identity: &str, payload: &[u8]) -> Option<ChatMessage> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        tracing::debug!(len = payload.len(), "dropping oversized chat payload");
        return None;
    }
    let mut message: ChatMessage = match serde_json::from_slice(payload) {
        Ok(message) => message,
        Err(err) => {
            tracing::debug!(%err, "dropping malformed chat payload");
            return None;
        }
    };
    if validate_chat_text(&message.text).is_err() || message.id.chars().count() > MAX_ID_LEN {
        tracing::debug!("dropping invalid chat message");
        return None;
    }
    message.sender = sender_identity.to_owned();
    Some(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(sender: &str, text: &str) -> Vec<u8> {
        let message = ChatMessage {
            id: "1".to_owned(),
            sender: sender.to_owned(),
            sent_at_unix_ms: 5,
            text: text.to_owned(),
        };
        serde_json::to_vec(&message).unwrap_or_default()
    }

    #[test]
    fn sender_is_overwritten() {
        let parsed = parse_incoming("real", &payload("spoofed", "oi"));
        assert_eq!(parsed.map(|m| m.sender), Some("real".to_owned()));
    }

    #[test]
    fn invalid_payloads_are_dropped() {
        assert!(parse_incoming("a", b"not json").is_none());
        assert!(parse_incoming("a", &payload("a", "   ")).is_none());
        assert!(parse_incoming("a", &payload("a", &"x".repeat(2001))).is_none());
        assert!(parse_incoming("a", &vec![b' '; MAX_PAYLOAD_BYTES + 1]).is_none());
    }

    #[test]
    fn outgoing_rejects_empty_text() {
        assert!(build_outgoing("1".to_owned(), "me", " ", 1).is_err());
    }
}
