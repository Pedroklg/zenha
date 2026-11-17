//! Interface text and classification of session errors.

use zenha_core::{CaptureEnd, SessionError};

pub(super) fn describe(error: &SessionError) -> String {
    match error {
        SessionError::InviteRejected => "Senha recusada pelo servidor.".to_owned(),
        SessionError::RoomLocked(_) => "Esta sala tem senha.".to_owned(),
        SessionError::RoomPasswordRejected => "Senha da sala incorreta.".to_owned(),
        SessionError::RoomUnlockRateLimited => {
            "Muitas tentativas de senha. Espere um pouco e tente de novo.".to_owned()
        }
        SessionError::TokenServer(reason) => {
            format!("Não foi possível falar com o servidor de tokens: {reason}")
        }
        SessionError::Connection(reason) => {
            format!("Falha na conexão com o servidor de mídia: {reason}")
        }
        SessionError::ShareUnavailable => {
            "A transmissão de tela não está disponível nesta versão.".to_owned()
        }
        SessionError::ShareActive => "Já existe uma transmissão em andamento.".to_owned(),
        SessionError::TargetNotFound(_) => {
            "A tela ou janela escolhida não está mais disponível.".to_owned()
        }
        SessionError::CaptureEnded(reason) => match reason {
            CaptureEnd::TargetClosed => {
                "A transmissão terminou: a tela ou janela foi fechada ou ficou indisponível."
                    .to_owned()
            }
            CaptureEnd::RepeatedErrors => {
                "A transmissão terminou: a captura falhou várias vezes seguidas.".to_owned()
            }
            CaptureEnd::Failed(detail) => {
                format!("A transmissão terminou: a captura falhou ({detail}).")
            }
        },
        SessionError::Video(detail) => format!("Falha no vídeo: {detail}"),
        SessionError::RateLimited => {
            "Muitas mensagens seguidas. Espere alguns segundos.".to_owned()
        }
        SessionError::Unauthorized => {
            "Não foi possível salvar: sessão expirada. Tente de novo.".to_owned()
        }
        SessionError::Chat(_) => {
            "O servidor não respondeu ao chat salvo. A mensagem não foi salva; tente de novo."
                .to_owned()
        }
        SessionError::Avatar(_) => {
            "Não foi possível atualizar a foto. Use PNG, JPEG ou WebP de até 2 MB e tente de novo."
                .to_owned()
        }
        SessionError::AdminInvalidPassword => "Senha de administrador incorreta.".to_owned(),
        SessionError::AdminUnauthorized => {
            "Sessão de administrador expirada. Entre novamente.".to_owned()
        }
        SessionError::AdminDisabled => {
            "O modo administrador não está habilitado neste servidor.".to_owned()
        }
        SessionError::AdminForbidden => "Sem permissão de administrador.".to_owned(),
        SessionError::AdminRateLimited => {
            "Muitas tentativas de login de administrador. Aguarde antes de tentar novamente."
                .to_owned()
        }
        SessionError::AdminTargetNotFound(_) => {
            "Sala, participante ou mensagem não encontrado no servidor.".to_owned()
        }
        SessionError::Admin(detail) => {
            if detail == "cannot delete the last room" {
                "Não é possível apagar a última sala.".to_owned()
            } else {
                format!("Falha na ação de administrador: {detail}")
            }
        }
        other => other.to_string(),
    }
}

pub(super) fn is_admin_error(error: &SessionError) -> bool {
    matches!(
        error,
        SessionError::AdminInvalidPassword
            | SessionError::AdminUnauthorized
            | SessionError::AdminDisabled
            | SessionError::AdminForbidden
            | SessionError::AdminRateLimited
            | SessionError::AdminTargetNotFound(_)
            | SessionError::Admin(_)
    )
}

pub(super) fn is_share_error(error: &SessionError) -> bool {
    matches!(
        error,
        SessionError::ShareUnavailable
            | SessionError::ShareActive
            | SessionError::TargetNotFound(_)
            | SessionError::CaptureEnded(_)
            | SessionError::Video(_)
    )
}
