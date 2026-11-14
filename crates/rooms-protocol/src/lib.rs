//! Tipos trocados entre `rooms-client` e `rooms-token-server`.
//!
//! Esta crate não faz I/O nem depende de runtime assíncrono.

use serde::{Deserialize, Serialize};

pub const MAX_DISPLAY_NAME_LEN: usize = 32;
pub const MAX_ROOM_NAME_LEN: usize = 32;
pub const MAX_CLIENT_ID_LEN: usize = 64;
pub const MAX_CHAT_MESSAGE_LEN: usize = 2000;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_FILE_NAME_LEN: usize = 120;
pub const MAX_CHAT_ID_LEN: usize = 64;
pub const CHAT_HISTORY_PAGE: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRequest {
    pub client_id: String,
    pub display_name: String,
    pub room: String,
    pub invite_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    pub livekit_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSummary {
    pub name: String,
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomsResponse {
    pub rooms: Vec<RoomSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub sender: String,
    pub sent_at_unix_ms: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveChatRequest {
    pub id: String,
    pub text: String,
    pub sent_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedChatMessage {
    pub message: ChatMessage,
    pub sender_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryResponse {
    pub messages: Vec<SavedChatMessage>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDeleted {
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("value is empty")]
    Empty,
    #[error("value is longer than {max} characters")]
    TooLong { max: usize },
    #[error("value contains a character that is not allowed")]
    InvalidCharacter,
}

/// Nome de exibição: 1 a 32 caracteres depois de aparar, sem caracteres de controle.
pub fn validate_display_name(name: &str) -> Result<(), ValidationError> {
    let name = name.trim();
    check_len(name, MAX_DISPLAY_NAME_LEN)?;
    if name.chars().any(char::is_control) {
        return Err(ValidationError::InvalidCharacter);
    }
    Ok(())
}

/// Nome de sala: 1 a 32 caracteres entre `a-z`, `0-9`, `-` e `_`.
pub fn validate_room_name(room: &str) -> Result<(), ValidationError> {
    check_len(room, MAX_ROOM_NAME_LEN)?;
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
    if !room.chars().all(allowed) {
        return Err(ValidationError::InvalidCharacter);
    }
    Ok(())
}

/// Identificador do cliente: 1 a 64 caracteres entre `a-z`, `0-9` e `-`.
pub fn validate_client_id(id: &str) -> Result<(), ValidationError> {
    check_len(id, MAX_CLIENT_ID_LEN)?;
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
    if !id.chars().all(allowed) {
        return Err(ValidationError::InvalidCharacter);
    }
    Ok(())
}

/// Texto de chat: não vazio depois de aparar e com até 2000 caracteres.
pub fn validate_chat_text(text: &str) -> Result<(), ValidationError> {
    check_len(text.trim(), MAX_CHAT_MESSAGE_LEN)
}

fn check_len(value: &str, max: usize) -> Result<(), ValidationError> {
    match value.chars().count() {
        0 => Err(ValidationError::Empty),
        len if len > max => Err(ValidationError::TooLong { max }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_request_round_trips_through_json() {
        let request = TokenRequest {
            client_id: "0123456789abcdef0123456789abcdef".to_owned(),
            display_name: "pedro".to_owned(),
            room: "geral".to_owned(),
            invite_code: "codigo".to_owned(),
        };
        let json = serde_json::to_string(&request).unwrap();
        let parsed: TokenRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, request);
    }

    #[test]
    fn chat_message_round_trips_through_json() {
        let message = ChatMessage {
            id: "1".to_owned(),
            sender: "pedro".to_owned(),
            sent_at_unix_ms: 1_700_000_000_000,
            text: "olá".to_owned(),
            saved: false,
        };
        let json = serde_json::to_string(&message).unwrap();
        let parsed: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, message);
    }

    #[test]
    fn unsaved_chat_message_omits_the_flag_and_old_payloads_still_parse() {
        let json = r#"{"id":"1","sender":"ana","sent_at_unix_ms":5,"text":"oi"}"#;
        let parsed: ChatMessage = serde_json::from_str(json).unwrap();
        assert!(!parsed.saved);
        assert!(!serde_json::to_string(&parsed).unwrap().contains("saved"));
    }

    #[test]
    fn chat_history_types_round_trip_through_json() {
        let history = ChatHistoryResponse {
            messages: vec![SavedChatMessage {
                message: ChatMessage {
                    id: "a-1".to_owned(),
                    sender: "Ana".to_owned(),
                    sent_at_unix_ms: 7,
                    text: "fica salvo".to_owned(),
                    saved: true,
                },
                sender_id: "0123".to_owned(),
            }],
            has_more: true,
        };
        let json = serde_json::to_string(&history).unwrap();
        assert_eq!(
            serde_json::from_str::<ChatHistoryResponse>(&json).unwrap(),
            history
        );

        let request = SaveChatRequest {
            id: "a-1".to_owned(),
            text: "fica salvo".to_owned(),
            sent_at_unix_ms: 7,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<SaveChatRequest>(&json).unwrap(),
            request
        );

        let deleted = ChatDeleted {
            id: "a-1".to_owned(),
        };
        let json = serde_json::to_string(&deleted).unwrap();
        assert_eq!(serde_json::from_str::<ChatDeleted>(&json).unwrap(), deleted);
    }

    #[test]
    fn display_name_rules() {
        assert_eq!(validate_display_name("Pedro"), Ok(()));
        assert_eq!(validate_display_name("   "), Err(ValidationError::Empty));
        assert_eq!(
            validate_display_name(&"a".repeat(33)),
            Err(ValidationError::TooLong {
                max: MAX_DISPLAY_NAME_LEN
            })
        );
        assert_eq!(
            validate_display_name("a\nb"),
            Err(ValidationError::InvalidCharacter)
        );
    }

    #[test]
    fn room_name_rules() {
        assert_eq!(validate_room_name("geral-2"), Ok(()));
        assert_eq!(validate_room_name(""), Err(ValidationError::Empty));
        assert_eq!(
            validate_room_name("Geral"),
            Err(ValidationError::InvalidCharacter)
        );
        assert_eq!(
            validate_room_name("a b"),
            Err(ValidationError::InvalidCharacter)
        );
    }

    #[test]
    fn client_id_rules() {
        assert_eq!(validate_client_id("a"), Ok(()));
        assert_eq!(validate_client_id("bot-1-abc09"), Ok(()));
        assert_eq!(validate_client_id(&"a".repeat(64)), Ok(()));
        assert_eq!(validate_client_id(""), Err(ValidationError::Empty));
        assert_eq!(
            validate_client_id(&"a".repeat(65)),
            Err(ValidationError::TooLong {
                max: MAX_CLIENT_ID_LEN
            })
        );
        for bad in ["Bot", "bot_1", "bot 1", "bot\n", "hé"] {
            assert_eq!(
                validate_client_id(bad),
                Err(ValidationError::InvalidCharacter),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn file_limits() {
        assert_eq!(MAX_FILE_BYTES, 52_428_800);
        assert_eq!(MAX_FILE_NAME_LEN, 120);
    }

    #[test]
    fn chat_text_rules() {
        assert_eq!(validate_chat_text("oi"), Ok(()));
        assert_eq!(validate_chat_text("  \n "), Err(ValidationError::Empty));
        assert_eq!(
            validate_chat_text(&"é".repeat(2001)),
            Err(ValidationError::TooLong {
                max: MAX_CHAT_MESSAGE_LEN
            })
        );
    }
}
