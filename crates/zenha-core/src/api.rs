use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use zenha_protocol::{AdminRoom, ChatMessage, RoomSummary, SavedChatMessage};

use crate::capture::CaptureEnd;
use crate::cue::Cue;
use crate::video::{CaptureTarget, FrameSlot, SharePreset};

#[derive(Clone, PartialEq, Eq)]
pub struct SessionConfig {
    pub token_server_url: String,
    pub client_id: String,
    pub display_name: String,
    pub invite_code: String,
    pub avatar_version: Option<String>,
    pub avatar_cache_dir: Option<PathBuf>,
    /// Chaves de salas com senha, por nome da sala dona da senha.
    pub room_keys: BTreeMap<String, String>,
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionConfig")
            .field("token_server_url", &self.token_server_url)
            .field("client_id", &self.client_id)
            .field("display_name", &self.display_name)
            .field("invite_code", &"<redacted>")
            .field("avatar_version", &self.avatar_version)
            .field("avatar_cache_dir", &self.avatar_cache_dir)
            .field("room_keys", &self.room_keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Chave de acesso a uma sala com senha; o `Debug` não mostra o valor.
#[derive(Clone, PartialEq, Eq)]
pub struct RoomKey(pub String);

impl fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoomKey(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub identity: String,
    pub name: String,
    pub muted: bool,
    pub speaking: bool,
    pub avatar_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// Tratamento de ruído do microfone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoiseSuppression {
    Off,
    /// Supressão embutida do WebRTC.
    #[default]
    Standard,
    /// Rede neural (RNNoise) num caminho de captura próprio; sem cancelamento de eco.
    Enhanced,
    /// Rede DeepFilterNet 3 no mesmo caminho de captura próprio do modo Reforçada; mais pesada.
    Maximum,
}

impl NoiseSuppression {
    pub fn uses_own_capture(self) -> bool {
        matches!(self, Self::Enhanced | Self::Maximum)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransfer {
    pub id: String,
    /// Identidade de quem envia (como em `ChatMessage::sender` recebido).
    pub sender: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub outgoing: bool,
    pub sent_at_unix_ms: u64,
    pub state: TransferState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    InProgress {
        bytes: u64,
    },
    /// Onde o arquivo está neste computador: o original (envio) ou a cópia recebida.
    Done {
        path: PathBuf,
    },
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDevices {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
    pub selected_input: Option<String>,
    pub selected_output: Option<String>,
}

/// Motivo pelo qual o usuário foi removido de uma sala pelo servidor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalReason {
    /// Removido por um administrador.
    Kicked,
    /// A sala em que estava foi apagada.
    RoomDeleted,
}

/// Ações administrativas executadas no servidor de tokens.
#[derive(Clone, PartialEq, Eq)]
pub enum AdminAction {
    CreateRoom {
        title: String,
        password: Option<String>,
        parent: Option<String>,
    },
    /// Em `password` e `parent`, `Some("")` remove a senha ou a sala-mãe.
    UpdateRoom {
        id: String,
        title: Option<String>,
        position: Option<usize>,
        password: Option<String>,
        parent: Option<String>,
    },
    DeleteRoom {
        id: String,
    },
    MuteParticipant {
        room: String,
        identity: String,
    },
    StopShareParticipant {
        room: String,
        identity: String,
    },
    KickParticipant {
        room: String,
        identity: String,
    },
    MoveParticipant {
        room: String,
        identity: String,
        to_room: String,
    },
    DeleteChat {
        room: String,
        id: String,
    },
    RotateInvite {
        new_code: String,
    },
}

impl fmt::Debug for AdminAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateRoom {
                title,
                password,
                parent,
            } => f
                .debug_struct("CreateRoom")
                .field("title", title)
                .field("password", &password.as_ref().map(|_| "<redacted>"))
                .field("parent", parent)
                .finish(),
            Self::UpdateRoom {
                id,
                title,
                position,
                password,
                parent,
            } => f
                .debug_struct("UpdateRoom")
                .field("id", id)
                .field("title", title)
                .field("position", position)
                .field("password", &password.as_ref().map(|_| "<redacted>"))
                .field("parent", parent)
                .finish(),
            Self::DeleteRoom { id } => f.debug_struct("DeleteRoom").field("id", id).finish(),
            Self::MuteParticipant { room, identity } => f
                .debug_struct("MuteParticipant")
                .field("room", room)
                .field("identity", identity)
                .finish(),
            Self::StopShareParticipant { room, identity } => f
                .debug_struct("StopShareParticipant")
                .field("room", room)
                .field("identity", identity)
                .finish(),
            Self::KickParticipant { room, identity } => f
                .debug_struct("KickParticipant")
                .field("room", room)
                .field("identity", identity)
                .finish(),
            Self::MoveParticipant {
                room,
                identity,
                to_room,
            } => f
                .debug_struct("MoveParticipant")
                .field("room", room)
                .field("identity", identity)
                .field("to_room", to_room)
                .finish(),
            Self::DeleteChat { room, id } => f
                .debug_struct("DeleteChat")
                .field("room", room)
                .field("id", id)
                .finish(),
            Self::RotateInvite { .. } => f
                .debug_struct("RotateInvite")
                .field("new_code", &"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Command {
    Join {
        room: String,
    },
    /// Troca a senha de uma sala por uma chave de acesso, guardada na sessão.
    UnlockRoom {
        room: String,
        password: String,
    },
    Leave,
    SetMuted(bool),
    SetDeafened(bool),
    /// Troca o nome exibido: vale para os próximos tokens e, numa sala, para os outros na hora.
    SetDisplayName {
        name: String,
    },
    /// Envia uma nova foto de perfil para o servidor e propaga a versão pela sala.
    SetAvatar {
        path: PathBuf,
    },
    /// Remove a foto de perfil no servidor e limpa o atributo na sala.
    ClearAvatar,
    SendChat {
        text: String,
        save: bool,
    },
    LoadChatHistory {
        before: Option<String>,
    },
    DeleteChat {
        id: String,
    },
    SendFile {
        path: PathBuf,
    },
    RefreshRooms,
    RefreshAudioDevices,
    SetInputDevice(Option<String>),
    SetOutputDevice(Option<String>),
    SetNoiseSuppression(NoiseSuppression),
    /// Volume do som da transmissão assistida, de 0 a 100.
    SetStreamVolume(u8),
    /// Volume de um participante remoto, de 0 a 200 (100 é o padrão).
    SetParticipantVolume {
        identity: String,
        percent: u16,
    },
    /// Limiar da porta de ruído em dBFS, de -70 a -20; `None` a desliga.
    SetNoiseGate(Option<i8>),
    RefreshCaptureTargets,
    /// Interrompe a geração de miniaturas de captura em andamento.
    CancelCaptureTargets,
    StartScreenShare {
        target_id: String,
        preset: SharePreset,
    },
    StopScreenShare,
    WatchScreenShare {
        identity: String,
    },
    StopWatching,
    /// Informa ao núcleo se a janela está visível para pular conversões de vídeo ocultas.
    SetUiVisible(bool),
    /// Som curto de confirmação, só para quem usa o app.
    PlayCue(Cue),
    /// Autentica como administrador no servidor de tokens (token mantido só em memória).
    AdminLogin {
        password: String,
    },
    /// Encerra a sessão administrativa local sem sair da sala atual.
    AdminLogout,
    /// Executa uma ação administrativa autenticada.
    Admin(AdminAction),
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Join { room } => f.debug_struct("Join").field("room", room).finish(),
            Self::UnlockRoom { room, .. } => f
                .debug_struct("UnlockRoom")
                .field("room", room)
                .field("password", &"<redacted>")
                .finish(),
            Self::Leave => f.write_str("Leave"),
            Self::SetMuted(muted) => f.debug_tuple("SetMuted").field(muted).finish(),
            Self::SetDeafened(deafened) => f.debug_tuple("SetDeafened").field(deafened).finish(),
            Self::SetDisplayName { name } => f
                .debug_struct("SetDisplayName")
                .field("name", name)
                .finish(),
            Self::SetAvatar { path } => f.debug_struct("SetAvatar").field("path", path).finish(),
            Self::ClearAvatar => f.write_str("ClearAvatar"),
            Self::SendChat { text, save } => f
                .debug_struct("SendChat")
                .field("text", text)
                .field("save", save)
                .finish(),
            Self::LoadChatHistory { before } => f
                .debug_struct("LoadChatHistory")
                .field("before", before)
                .finish(),
            Self::DeleteChat { id } => f.debug_struct("DeleteChat").field("id", id).finish(),
            Self::SendFile { path } => f.debug_struct("SendFile").field("path", path).finish(),
            Self::RefreshRooms => f.write_str("RefreshRooms"),
            Self::RefreshAudioDevices => f.write_str("RefreshAudioDevices"),
            Self::SetInputDevice(id) => f.debug_tuple("SetInputDevice").field(id).finish(),
            Self::SetOutputDevice(id) => f.debug_tuple("SetOutputDevice").field(id).finish(),
            Self::SetNoiseSuppression(mode) => {
                f.debug_tuple("SetNoiseSuppression").field(mode).finish()
            }
            Self::SetStreamVolume(percent) => {
                f.debug_tuple("SetStreamVolume").field(percent).finish()
            }
            Self::SetParticipantVolume { identity, percent } => f
                .debug_struct("SetParticipantVolume")
                .field("identity", identity)
                .field("percent", percent)
                .finish(),
            Self::SetNoiseGate(db) => f.debug_tuple("SetNoiseGate").field(db).finish(),
            Self::RefreshCaptureTargets => f.write_str("RefreshCaptureTargets"),
            Self::StartScreenShare { target_id, preset } => f
                .debug_struct("StartScreenShare")
                .field("target_id", target_id)
                .field("preset", preset)
                .finish(),
            Self::StopScreenShare => f.write_str("StopScreenShare"),
            Self::WatchScreenShare { identity } => f
                .debug_struct("WatchScreenShare")
                .field("identity", identity)
                .finish(),
            Self::StopWatching => f.write_str("StopWatching"),
            Self::PlayCue(cue) => f.debug_tuple("PlayCue").field(cue).finish(),
            Self::CancelCaptureTargets => f.write_str("CancelCaptureTargets"),
            Self::SetUiVisible(visible) => f.debug_tuple("SetUiVisible").field(visible).finish(),
            Self::AdminLogin { .. } => f
                .debug_struct("AdminLogin")
                .field("password", &"<redacted>")
                .finish(),
            Self::AdminLogout => f.write_str("AdminLogout"),
            Self::Admin(action) => f.debug_tuple("Admin").field(action).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connecting,
    Joined {
        room: String,
        local_identity: String,
        participants: Vec<Participant>,
    },
    Reconnecting,
    Reconnected,
    Left,
    Failed(SessionError),
    ParticipantList(Vec<Participant>),
    ParticipantJoined(Participant),
    ParticipantLeft {
        identity: String,
    },
    ParticipantMuteChanged {
        identity: String,
        muted: bool,
    },
    ParticipantSpeakingChanged {
        identity: String,
        speaking: bool,
    },
    ParticipantNameChanged {
        identity: String,
        name: String,
    },
    ParticipantAvatarChanged {
        identity: String,
        version: Option<String>,
    },
    LocalAvatarChanged {
        version: Option<String>,
    },
    AvatarCached {
        identity: String,
        version: String,
        path: PathBuf,
    },
    ChatReceived(ChatMessage),
    /// Página do histórico de mensagens salvas retornada pelo servidor.
    ChatHistory {
        messages: Vec<SavedChatMessage>,
        has_more: bool,
        before: Option<String>,
    },
    /// Indica que uma mensagem salva foi removida.
    ///
    /// A interface (T31c) deve conferir a autoria comparando `sender` com o
    /// `sender_id` ou identidade local que já possui antes de remover o item.
    /// Quando `sender` é vazio (`""`), a remoção veio de um comando administrativo do servidor.
    ChatDeleted {
        id: String,
        sender: String,
    },
    FileTransfer(FileTransfer),
    RoomsUpdated(Vec<RoomSummary>),
    /// Senha aceita; `room` é a sala dona da senha, que pode ser a sala-mãe da pedida.
    RoomUnlocked {
        room: String,
        key: RoomKey,
    },
    AudioDevicesUpdated(AudioDevices),
    CaptureTargetsUpdated(Vec<CaptureTarget>),
    /// Miniatura RGBA de um alvo de captura, emitida assim que ficar pronta.
    CaptureThumbnail {
        id: String,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// Nível do microfone no modo Reforçada, cerca de dez vezes por segundo.
    MicLevel {
        level_db: i8,
        gate_open: bool,
    },
    ScreenShareStarted {
        preview: FrameSlot,
    },
    ScreenShareStopped,
    RemoteScreenShareStarted {
        identity: String,
    },
    RemoteScreenShareStopped {
        identity: String,
    },
    WatchingStarted {
        identity: String,
        frames: FrameSlot,
    },
    WatchingStopped,
    /// Indica mudança no estado da sessão administrativa em memória.
    AdminChanged {
        active: bool,
    },
    /// Sala criada via ação administrativa.
    AdminRoomCreated(AdminRoom),
    /// Sala atualizada via ação administrativa.
    AdminRoomUpdated(AdminRoom),
    /// Sala removida via ação administrativa.
    AdminRoomDeleted {
        id: String,
    },
    /// Código de convite atualizado via ação administrativa.
    AdminInviteRotated,
    /// Um administrador encerrou a transmissão de tela local.
    AdminStoppedScreenShare,
    /// Um administrador moveu o usuário para outra sala.
    MovedByAdmin {
        to_room: String,
    },
    /// O servidor removeu o usuário da sala sem permitir reconexão automática.
    RemovedFromRoom {
        reason: RemovalReason,
    },
    CommandFailed(SessionError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("could not reach the token server: {0}")]
    TokenServer(String),
    #[error("invite code rejected")]
    InviteRejected,
    #[error("room {0} requires a password")]
    RoomLocked(String),
    #[error("wrong room password")]
    RoomPasswordRejected,
    #[error("too many room password attempts; try again later")]
    RoomUnlockRateLimited,
    #[error("room token rejected")]
    Unauthorized,
    #[error("too many chat messages; try again in a few seconds")]
    RateLimited,
    #[error("saved chat request failed: {0}")]
    Chat(String),
    #[error("avatar request failed: {0}")]
    Avatar(String),
    #[error("invalid admin password")]
    AdminInvalidPassword,
    #[error("admin session expired")]
    AdminUnauthorized,
    #[error("admin access is not enabled on this server")]
    AdminDisabled,
    #[error("admin permission denied")]
    AdminForbidden,
    #[error("too many admin login attempts; try again later")]
    AdminRateLimited,
    #[error("admin target not found: {0}")]
    AdminTargetNotFound(String),
    #[error("admin request failed: {0}")]
    Admin(String),
    #[error("connection to the media server failed: {0}")]
    Connection(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not in a room")]
    NotInRoom,
    #[error("screen sharing is not available in this build")]
    ShareUnavailable,
    #[error("a screen share is already active")]
    ShareActive,
    #[error("capture target not found: {0}")]
    TargetNotFound(String),
    #[error("screen capture ended: {0}")]
    CaptureEnded(CaptureEnd),
    #[error("video error: {0}")]
    Video(String),
    #[error("internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_enhanced_and_maximum_use_their_own_capture() {
        assert!(!NoiseSuppression::Off.uses_own_capture());
        assert!(!NoiseSuppression::Standard.uses_own_capture());
        assert!(NoiseSuppression::Enhanced.uses_own_capture());
        assert!(NoiseSuppression::Maximum.uses_own_capture());
    }

    #[test]
    fn command_debug_redacts_secrets() {
        let login = Command::AdminLogin {
            password: "super-secret-admin-pw".to_owned(),
        };
        assert!(!format!("{login:?}").contains("super-secret-admin-pw"));

        let invite = Command::Admin(AdminAction::RotateInvite {
            new_code: "secret-invite-code-999".to_owned(),
        });
        assert!(!format!("{invite:?}").contains("secret-invite-code-999"));

        let unlock = Command::UnlockRoom {
            room: "vip".to_owned(),
            password: "senha-da-sala".to_owned(),
        };
        assert!(!format!("{unlock:?}").contains("senha-da-sala"));
        let create = Command::Admin(AdminAction::CreateRoom {
            title: "VIP".to_owned(),
            password: Some("senha-da-sala".to_owned()),
            parent: None,
        });
        assert!(!format!("{create:?}").contains("senha-da-sala"));
        let unlocked = Event::RoomUnlocked {
            room: "vip".to_owned(),
            key: RoomKey("chave-secreta".to_owned()),
        };
        assert!(!format!("{unlocked:?}").contains("chave-secreta"));
        let config = SessionConfig {
            token_server_url: String::new(),
            client_id: String::new(),
            display_name: String::new(),
            invite_code: String::new(),
            avatar_version: None,
            avatar_cache_dir: None,
            room_keys: [("vip".to_owned(), "chave-secreta".to_owned())].into(),
        };
        assert!(!format!("{config:?}").contains("chave-secreta"));
    }
}
