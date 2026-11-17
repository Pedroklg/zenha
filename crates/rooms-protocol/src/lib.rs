//! Tipos trocados entre `rooms-client` e `rooms-token-server`.
//!
//! Esta crate não faz I/O nem depende de runtime assíncrono.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const MAX_DISPLAY_NAME_LEN: usize = 32;
pub const MAX_ROOM_NAME_LEN: usize = 32;
pub const MAX_ROOM_TITLE_LEN: usize = 64;
pub const MAX_ROOMS_COUNT: usize = 50;
pub const MAX_CLIENT_ID_LEN: usize = 64;
pub const MAX_CHAT_MESSAGE_LEN: usize = 2000;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_FILE_NAME_LEN: usize = 120;
pub const MAX_CHAT_ID_LEN: usize = 64;
pub const CHAT_HISTORY_PAGE: usize = 100;
pub const MIN_INVITE_CODE_LEN: usize = 8;
pub const MAX_INVITE_CODE_LEN: usize = 128;
pub const MAX_ROOM_PASSWORD_LEN: usize = 128;
/// Cabeçalho de `GET /rooms` com as chaves de salas trancadas: `sala=chave,sala=chave`.
pub const ROOM_KEYS_HEADER: &str = "x-room-keys";
/// Texto de erro de `POST /token` quando a sala tem senha e a chave falta ou não vale.
pub const ROOM_LOCKED_ERROR: &str = "room_locked";

pub const ADMIN_STOP_SHARE_TOPIC: &str = "admin.stop_share";
pub const ADMIN_MOVE_TOPIC: &str = "admin.move";
pub const ADMIN_CHAT_DELETE_TOPIC: &str = "admin.chat_delete";
pub const ROOMS_CHANGED_TOPIC: &str = "rooms.changed";
pub const AVATAR_ATTR: &str = "zenha.avatar";
pub const AVATAR_SIZE_PX: u32 = 128;
pub const MAX_AVATAR_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_AVATAR_VERSION_LEN: usize = 32;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRequest {
    pub client_id: String,
    pub display_name: String,
    pub room: String,
    pub invite_code: String,
    /// Chave de acesso da sala ou da sala-mãe, exigida quando a sala tem senha.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_key: Option<String>,
}

impl fmt::Debug for TokenRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenRequest")
            .field("client_id", &self.client_id)
            .field("display_name", &self.display_name)
            .field("room", &self.room)
            .field("invite_code", &"<redacted>")
            .field("room_key", &self.room_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    pub livekit_url: String,
}

impl fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenResponse")
            .field("token", &"<redacted>")
            .field("livekit_url", &self.livekit_url)
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Vazio numa sala trancada sem chave válida; a contagem fica em `participant_count`.
    pub participants: Vec<String>,
    #[serde(default)]
    pub participant_count: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unlocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

impl RoomSummary {
    pub fn occupant_count(&self) -> usize {
        self.participant_count.max(self.participants.len())
    }

    /// Sala com senha que este cliente ainda não desbloqueou.
    pub fn needs_password(&self) -> bool {
        self.locked && !self.unlocked
    }

    /// Nome exibido da sala, usando o identificador técnico como fallback para payloads antigos.
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            &self.name
        } else {
            &self.title
        }
    }
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminLoginRequest {
    pub password: String,
    pub client_id: String,
}

impl fmt::Debug for AdminLoginRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminLoginRequest")
            .field("client_id", &self.client_id)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminLoginResponse {
    pub token: String,
    /// Expiração do token em segundos desde a época Unix (igual à claim `exp` do JWT).
    pub expires_at: u64,
}

impl fmt::Debug for AdminLoginResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminLoginResponse")
            .field("expires_at", &self.expires_at)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRoom {
    pub id: String,
    pub title: String,
    pub position: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRoomRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

impl fmt::Debug for CreateRoomRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateRoomRequest")
            .field("title", &self.title)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("parent", &self.parent)
            .finish()
    }
}

/// Campos ausentes ficam como estão. `password` e `parent` vazios (`""`) tiram a senha e a sala-mãe.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRoomRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

impl fmt::Debug for UpdateRoomRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpdateRoomRequest")
            .field("title", &self.title)
            .field("position", &self.position)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("parent", &self.parent)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockRoomRequest {
    pub invite_code: String,
    pub password: String,
}

impl fmt::Debug for UnlockRoomRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnlockRoomRequest")
            .field("invite_code", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// `room` é a sala dona da senha que bateu: a pedida ou a sala-mãe dela.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockRoomResponse {
    pub room: String,
    pub room_key: String,
}

impl fmt::Debug for UnlockRoomResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnlockRoomResponse")
            .field("room", &self.room)
            .field("room_key", &"<redacted>")
            .finish()
    }
}

/// Monta o valor do cabeçalho `x-room-keys`.
pub fn encode_room_keys<'a>(keys: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    keys.into_iter()
        .map(|(room, key)| format!("{room}={key}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Lê o cabeçalho `x-room-keys`, ignorando pares malformados.
pub fn parse_room_keys(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(room, key)| validate_room_name(room).is_ok() && !key.is_empty())
        .map(|(room, key)| (room.to_owned(), key.to_owned()))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveParticipantRequest {
    pub to_room: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInviteRequest {
    pub new_code: String,
}

impl fmt::Debug for UpdateInviteRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpdateInviteRequest")
            .field("new_code", &"<redacted>")
            .finish()
    }
}

/// Comando do servidor no tópico `admin.stop_share` para o cliente encerrar a captura e transmissão de tela.
///
/// Segurança: o cliente só deve aceitar mensagens deste tópico quando vierem do servidor (sem
/// participante remetente), nunca de outro participante.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminStopShare {}

/// Comando do servidor no tópico `admin.move` para o cliente trocar de sala.
///
/// Segurança: o cliente só deve aceitar mensagens deste tópico quando vierem do servidor (sem
/// participante remetente), nunca de outro participante.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminMove {
    pub to_room: String,
    /// Chave da sala de destino quando ela tem senha, para o movido entrar sem digitá-la.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlock: Option<UnlockRoomResponse>,
}

/// Comando do servidor no tópico `admin.chat_delete` avisando a sala que uma mensagem salva foi removida.
///
/// Segurança: o cliente só deve aceitar mensagens deste tópico quando vierem do servidor (sem
/// participante remetente), nunca de outro participante.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChatDelete {
    pub id: String,
}

/// Aviso do servidor no tópico `rooms.changed` indicando que a lista ou ordem de salas mudou.
///
/// Segurança: o cliente só deve aceitar mensagens deste tópico quando vierem do servidor (sem
/// participante remetente), nunca de outro participante.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomsChanged {}

/// Resposta do servidor ao enviar uma foto de perfil com `PUT /avatar`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvatarUploadResponse {
    pub version: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InviteCodeError {
    #[error("invite code is empty")]
    Empty,
    #[error("invite code must be at least {min} characters")]
    TooShort { min: usize },
    #[error("invite code is longer than {max} characters")]
    TooLong { max: usize },
    #[error("invite code contains a character that is not allowed")]
    InvalidCharacter,
}

/// Nome de exibição: 1 a 32 caracteres depois de aparar, sem caracteres de controle.
pub fn validate_display_name(name: &str) -> Result<(), ValidationError> {
    validate_trimmed_text(name, MAX_DISPLAY_NAME_LEN)
}

/// Título exibido da sala: 1 a 64 caracteres depois de aparar, sem caracteres de controle.
pub fn validate_room_title(title: &str) -> Result<(), ValidationError> {
    validate_trimmed_text(title, MAX_ROOM_TITLE_LEN)
}

fn validate_trimmed_text(value: &str, max: usize) -> Result<(), ValidationError> {
    let trimmed = value.trim();
    check_len(trimmed, max)?;
    if trimmed.chars().any(char::is_control) {
        return Err(ValidationError::InvalidCharacter);
    }
    Ok(())
}

/// Senha de sala: 1 a 128 caracteres, não só espaços, sem caracteres de controle.
pub fn validate_room_password(password: &str) -> Result<(), ValidationError> {
    check_len(password, MAX_ROOM_PASSWORD_LEN)?;
    if password.trim().is_empty() {
        return Err(ValidationError::Empty);
    }
    if password.chars().any(char::is_control) {
        return Err(ValidationError::InvalidCharacter);
    }
    Ok(())
}

/// Código de convite: 8 a 128 caracteres, não vazio depois de aparar, sem caracteres de controle.
pub fn validate_invite_code(code: &str) -> Result<(), InviteCodeError> {
    if code.trim().is_empty() {
        return Err(InviteCodeError::Empty);
    }
    if code.chars().any(char::is_control) {
        return Err(InviteCodeError::InvalidCharacter);
    }
    let len = code.chars().count();
    if len < MIN_INVITE_CODE_LEN {
        return Err(InviteCodeError::TooShort {
            min: MIN_INVITE_CODE_LEN,
        });
    }
    if len > MAX_INVITE_CODE_LEN {
        return Err(InviteCodeError::TooLong {
            max: MAX_INVITE_CODE_LEN,
        });
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

/// Versão de foto de perfil: 1 a 32 caracteres ASCII seguros para URL e nome de arquivo.
pub fn validate_avatar_version(version: &str) -> Result<(), ValidationError> {
    check_len(version, MAX_AVATAR_VERSION_LEN)?;
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
    if !version.chars().all(allowed) {
        return Err(ValidationError::InvalidCharacter);
    }
    Ok(())
}

/// Interpreta o atributo `zenha.avatar`, tratando vazio ou inválido como ausência de foto.
pub fn parse_avatar_attribute(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    validate_avatar_version(trimmed)
        .ok()
        .map(|()| trimmed.to_owned())
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
            room_key: Some("abc".to_owned()),
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

    #[test]
    fn room_summary_is_backward_compatible_with_missing_title() {
        let legacy = r#"{"name":"geral","participants":["ana"]}"#;
        let parsed: RoomSummary = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            parsed,
            RoomSummary {
                name: "geral".to_owned(),
                participants: vec!["ana".to_owned()],
                ..RoomSummary::default()
            }
        );
        assert_eq!(parsed.occupant_count(), 1);
        assert!(!parsed.needs_password());
        assert_eq!(parsed.display_title(), "geral");
        assert!(!serde_json::to_string(&parsed).unwrap().contains("title"));

        let with_title = RoomSummary {
            name: "geral".to_owned(),
            title: "Sala Geral".to_owned(),
            participants: vec!["ana".to_owned()],
            ..RoomSummary::default()
        };
        assert_eq!(with_title.display_title(), "Sala Geral");
        let json = serde_json::to_string(&with_title).unwrap();
        assert_eq!(
            serde_json::from_str::<RoomSummary>(&json).unwrap(),
            with_title
        );
    }

    #[test]
    fn admin_http_types_round_trip_through_json_and_redact_secrets() {
        let login = AdminLoginRequest {
            password: "secret-password".to_owned(),
            client_id: "client-admin".to_owned(),
        };
        let login_json = serde_json::to_string(&login).unwrap();
        assert_eq!(
            serde_json::from_str::<AdminLoginRequest>(&login_json).unwrap(),
            login
        );
        assert!(!format!("{login:?}").contains("secret-password"));

        let login_res = AdminLoginResponse {
            token: "jwt-token-secret".to_owned(),
            expires_at: 1_800_000_000,
        };
        let login_res_json = serde_json::to_string(&login_res).unwrap();
        assert_eq!(
            serde_json::from_str::<AdminLoginResponse>(&login_res_json).unwrap(),
            login_res
        );
        assert!(!format!("{login_res:?}").contains("jwt-token-secret"));

        let admin_room = AdminRoom {
            id: "geral".to_owned(),
            title: "Sala Geral".to_owned(),
            position: 0,
            locked: true,
            parent: Some("base".to_owned()),
        };
        let room_json = serde_json::to_string(&admin_room).unwrap();
        assert_eq!(
            serde_json::from_str::<AdminRoom>(&room_json).unwrap(),
            admin_room
        );

        let create = CreateRoomRequest {
            title: "Nova Sala".to_owned(),
            password: Some("senha-da-sala".to_owned()),
            parent: Some("geral".to_owned()),
        };
        assert!(!format!("{create:?}").contains("senha-da-sala"));
        let create_json = serde_json::to_string(&create).unwrap();
        assert_eq!(
            serde_json::from_str::<CreateRoomRequest>(&create_json).unwrap(),
            create
        );

        let patch_empty: UpdateRoomRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(patch_empty, UpdateRoomRequest::default());
        let patch_full = UpdateRoomRequest {
            title: Some("Renomeada".to_owned()),
            position: Some(2),
            password: Some("outra-senha".to_owned()),
            parent: Some(String::new()),
        };
        assert!(!format!("{patch_full:?}").contains("outra-senha"));
        let patch_json = serde_json::to_string(&patch_full).unwrap();
        assert_eq!(
            serde_json::from_str::<UpdateRoomRequest>(&patch_json).unwrap(),
            patch_full
        );

        let mv = MoveParticipantRequest {
            to_room: "jogos".to_owned(),
        };
        let mv_json = serde_json::to_string(&mv).unwrap();
        assert_eq!(
            serde_json::from_str::<MoveParticipantRequest>(&mv_json).unwrap(),
            mv
        );

        let invite = UpdateInviteRequest {
            new_code: "novo-convite-seguro".to_owned(),
        };
        let invite_json = serde_json::to_string(&invite).unwrap();
        assert_eq!(
            serde_json::from_str::<UpdateInviteRequest>(&invite_json).unwrap(),
            invite
        );
        assert!(!format!("{invite:?}").contains("novo-convite-seguro"));
    }

    #[test]
    fn admin_data_topics_round_trip_through_json() {
        let stop = AdminStopShare {};
        let stop_json = serde_json::to_string(&stop).unwrap();
        assert_eq!(
            serde_json::from_str::<AdminStopShare>(&stop_json).unwrap(),
            stop
        );

        let mv = AdminMove {
            to_room: "afk".to_owned(),
            unlock: None,
        };
        let mv_json = serde_json::to_string(&mv).unwrap();
        assert_eq!(serde_json::from_str::<AdminMove>(&mv_json).unwrap(), mv);

        let del = AdminChatDelete {
            id: "msg-1".to_owned(),
        };
        let del_json = serde_json::to_string(&del).unwrap();
        assert_eq!(
            serde_json::from_str::<AdminChatDelete>(&del_json).unwrap(),
            del
        );

        let changed = RoomsChanged {};
        let changed_json = serde_json::to_string(&changed).unwrap();
        assert_eq!(
            serde_json::from_str::<RoomsChanged>(&changed_json).unwrap(),
            changed
        );
    }

    #[test]
    fn room_title_rules() {
        assert_eq!(validate_room_title("  Sala dos Amigos  "), Ok(()));
        assert_eq!(validate_room_title("   "), Err(ValidationError::Empty));
        assert_eq!(
            validate_room_title(&"a".repeat(MAX_ROOM_TITLE_LEN + 1)),
            Err(ValidationError::TooLong {
                max: MAX_ROOM_TITLE_LEN
            })
        );
        assert_eq!(
            validate_room_title("sala\nnova"),
            Err(ValidationError::InvalidCharacter)
        );
    }

    #[test]
    fn invite_code_rules() {
        assert_eq!(validate_invite_code("12345678"), Ok(()));
        assert_eq!(validate_invite_code(&"a".repeat(128)), Ok(()));
        assert_eq!(
            validate_invite_code("        "),
            Err(InviteCodeError::Empty)
        );
        assert_eq!(
            validate_invite_code("1234567"),
            Err(InviteCodeError::TooShort {
                min: MIN_INVITE_CODE_LEN
            })
        );
        assert_eq!(
            validate_invite_code(&"a".repeat(129)),
            Err(InviteCodeError::TooLong {
                max: MAX_INVITE_CODE_LEN
            })
        );
        assert_eq!(
            validate_invite_code("convite\n123"),
            Err(InviteCodeError::InvalidCharacter)
        );
    }

    #[test]
    fn token_types_debug_redacts_secrets() {
        let req = TokenRequest {
            client_id: "c1".to_owned(),
            display_name: "Ana".to_owned(),
            room: "geral".to_owned(),
            invite_code: "secret-invite-123".to_owned(),
            room_key: Some("secret-room-key".to_owned()),
        };
        let req_dbg = format!("{req:?}");
        assert!(!req_dbg.contains("secret-invite-123"));
        assert!(!req_dbg.contains("secret-room-key"));
        assert!(req_dbg.contains("Ana"));
        assert!(req_dbg.contains("<redacted>"));

        let resp = TokenResponse {
            token: "jwt-secret-token-xyz".to_owned(),
            livekit_url: "wss://lk.example".to_owned(),
        };
        let resp_dbg = format!("{resp:?}");
        assert!(!resp_dbg.contains("jwt-secret-token-xyz"));
        assert!(resp_dbg.contains("wss://lk.example"));
        assert!(resp_dbg.contains("<redacted>"));
    }

    #[test]
    fn avatar_upload_response_and_version_validation() {
        let resp = AvatarUploadResponse {
            version: "0123456789abcdef".to_owned(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            serde_json::from_str::<AvatarUploadResponse>(&json).unwrap(),
            resp
        );

        assert_eq!(validate_avatar_version("0123456789abcdef"), Ok(()));
        assert_eq!(validate_avatar_version(""), Err(ValidationError::Empty));
        assert_eq!(
            validate_avatar_version(&"a".repeat(MAX_AVATAR_VERSION_LEN + 1)),
            Err(ValidationError::TooLong {
                max: MAX_AVATAR_VERSION_LEN,
            })
        );
        assert_eq!(
            validate_avatar_version("../bad"),
            Err(ValidationError::InvalidCharacter)
        );
        assert_eq!(parse_avatar_attribute(""), None);
        assert_eq!(parse_avatar_attribute("   "), None);
        assert_eq!(parse_avatar_attribute("../escape"), None);
        assert_eq!(
            parse_avatar_attribute("0123456789abcdef"),
            Some("0123456789abcdef".to_owned())
        );
    }

    #[test]
    fn room_lock_types_round_trip_redact_and_stay_backward_compatible() {
        let locked = RoomSummary {
            name: "vip".to_owned(),
            participant_count: 3,
            locked: true,
            parent: Some("geral".to_owned()),
            ..RoomSummary::default()
        };
        let json = serde_json::to_string(&locked).unwrap();
        assert_eq!(serde_json::from_str::<RoomSummary>(&json).unwrap(), locked);
        assert_eq!(locked.occupant_count(), 3);
        assert!(locked.needs_password());

        let old_token = r#"{"client_id":"a","display_name":"b","room":"c","invite_code":"d"}"#;
        assert_eq!(
            serde_json::from_str::<TokenRequest>(old_token)
                .unwrap()
                .room_key,
            None
        );

        let unlock = UnlockRoomRequest {
            invite_code: "convite-secreto".to_owned(),
            password: "senha-secreta".to_owned(),
        };
        let json = serde_json::to_string(&unlock).unwrap();
        assert_eq!(
            serde_json::from_str::<UnlockRoomRequest>(&json).unwrap(),
            unlock
        );
        let dbg = format!("{unlock:?}");
        assert!(!dbg.contains("convite-secreto") && !dbg.contains("senha-secreta"));

        let unlocked = UnlockRoomResponse {
            room: "geral".to_owned(),
            room_key: "chave-secreta".to_owned(),
        };
        let json = serde_json::to_string(&unlocked).unwrap();
        assert_eq!(
            serde_json::from_str::<UnlockRoomResponse>(&json).unwrap(),
            unlocked
        );
        assert!(!format!("{unlocked:?}").contains("chave-secreta"));
    }

    #[test]
    fn room_keys_header_round_trips_and_skips_garbage() {
        let header = encode_room_keys([("geral", "aa11"), ("vip_2", "bb22")]);
        assert_eq!(header, "geral=aa11,vip_2=bb22");
        assert_eq!(
            parse_room_keys(&format!("{header}, ,Bad=1,semchave=,x")),
            vec![
                ("geral".to_owned(), "aa11".to_owned()),
                ("vip_2".to_owned(), "bb22".to_owned()),
            ]
        );
    }

    #[test]
    fn room_password_rules() {
        assert_eq!(validate_room_password("1"), Ok(()));
        assert_eq!(validate_room_password(""), Err(ValidationError::Empty));
        assert_eq!(
            validate_room_password(&"a".repeat(MAX_ROOM_PASSWORD_LEN + 1)),
            Err(ValidationError::TooLong {
                max: MAX_ROOM_PASSWORD_LEN
            })
        );
        assert_eq!(
            validate_room_password("a\tb"),
            Err(ValidationError::InvalidCharacter)
        );
    }
}
