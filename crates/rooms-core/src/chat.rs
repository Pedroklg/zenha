use rooms_protocol::{
    ChatDeleted, ChatMessage, MAX_CHAT_ID_LEN, MAX_CHAT_MESSAGE_LEN, validate_chat_text,
};

use crate::api::SessionError;

pub const CHAT_TOPIC: &str = "chat";
pub const CHAT_DELETE_TOPIC: &str = "chat.delete";
const MAX_PAYLOAD_BYTES: usize = MAX_CHAT_MESSAGE_LEN * 4 + 512;
const MAX_DELETE_PAYLOAD_BYTES: usize = MAX_CHAT_ID_LEN + 64;

pub fn validate_chat_id(id: &str) -> Result<(), SessionError> {
    let valid = !id.is_empty()
        && id.len() <= MAX_CHAT_ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(SessionError::InvalidInput("invalid message id".to_owned()))
    }
}

pub fn encode_message(message: &ChatMessage) -> Result<Vec<u8>, SessionError> {
    serde_json::to_vec(message).map_err(|err| SessionError::Internal(err.to_string()))
}

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
        saved: false,
    };
    let payload = encode_message(&message)?;
    Ok((message, payload))
}

pub fn build_delete_outgoing(id: &str) -> Result<Vec<u8>, SessionError> {
    validate_chat_id(id)?;
    let deleted = ChatDeleted { id: id.to_owned() };
    serde_json::to_vec(&deleted).map_err(|err| SessionError::Internal(err.to_string()))
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
    if validate_chat_text(&message.text).is_err() || message.id.chars().count() > MAX_CHAT_ID_LEN {
        tracing::debug!("dropping invalid chat message");
        return None;
    }
    message.sender = sender_identity.to_owned();
    Some(message)
}

pub fn parse_incoming_delete(payload: &[u8]) -> Option<String> {
    if payload.len() > MAX_DELETE_PAYLOAD_BYTES {
        tracing::debug!(
            len = payload.len(),
            "dropping oversized chat delete payload"
        );
        return None;
    }
    let deleted: ChatDeleted = match serde_json::from_slice(payload) {
        Ok(deleted) => deleted,
        Err(err) => {
            tracing::debug!(%err, "dropping malformed chat delete payload");
            return None;
        }
    };
    if validate_chat_id(&deleted.id).is_err() {
        tracing::debug!("dropping invalid chat delete id");
        return None;
    }
    Some(deleted.id)
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
            saved: false,
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

    #[test]
    fn incoming_preserves_saved_flag_while_overwriting_sender() {
        let message = ChatMessage {
            id: "msg-1".to_owned(),
            sender: "spoofed".to_owned(),
            sent_at_unix_ms: 42,
            text: "olá".to_owned(),
            saved: true,
        };
        let encoded = encode_message(&message).unwrap_or_default();
        let parsed = parse_incoming("real-peer", &encoded);
        assert_eq!(
            parsed,
            Some(ChatMessage {
                id: "msg-1".to_owned(),
                sender: "real-peer".to_owned(),
                sent_at_unix_ms: 42,
                text: "olá".to_owned(),
                saved: true,
            })
        );
    }

    #[test]
    fn delete_payload_validates_id_and_size() {
        let payload = build_delete_outgoing("msg_1-a").unwrap_or_default();
        assert_eq!(parse_incoming_delete(&payload), Some("msg_1-a".to_owned()));

        assert!(build_delete_outgoing("").is_err());
        assert!(build_delete_outgoing("../other").is_err());

        let empty = serde_json::to_vec(&ChatDeleted { id: String::new() }).unwrap_or_default();
        assert!(parse_incoming_delete(&empty).is_none());

        let oversized_id = serde_json::to_vec(&ChatDeleted {
            id: "a".repeat(MAX_CHAT_ID_LEN + 1),
        })
        .unwrap_or_default();
        assert!(parse_incoming_delete(&oversized_id).is_none());

        assert!(parse_incoming_delete(b"not json").is_none());
        assert!(parse_incoming_delete(&[b' '; MAX_DELETE_PAYLOAD_BYTES + 1]).is_none());
    }
}
