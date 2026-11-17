//! Envio, remoção e cache em disco das fotos de perfil na sessão.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zenha_protocol::{
    MAX_AVATAR_UPLOAD_BYTES, TokenRequest, validate_avatar_version, validate_client_id,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{Event, Participant, SessionConfig, SessionError};
use crate::net::token::TokenClient;

use super::Session;
use super::connection::Phase;
use super::rooms::room_key_for;

pub(super) enum AvatarTaskEvent {
    Uploaded {
        seq: u64,
        renewed_jwt: Option<(String, String)>,
        result: Result<Option<String>, SessionError>,
    },
    Downloaded {
        identity: String,
        version: String,
        renewed_jwt: Option<(String, String)>,
        result: Result<Option<PathBuf>, SessionError>,
    },
}

enum AvatarAuth {
    Room {
        room: String,
        jwt: String,
        room_key: Option<String>,
        config: SessionConfig,
    },
    Lobby {
        config: SessionConfig,
    },
}

impl AvatarAuth {
    fn from_session(session: &Session) -> Self {
        match &session.phase {
            Phase::Connected { room, jwt, .. } => Self::Room {
                room: room.clone(),
                jwt: jwt.clone(),
                room_key: session.room_key(room),
                config: session.config.clone(),
            },
            _ => Self::Lobby {
                config: session.config.clone(),
            },
        }
    }

    async fn run_with_jwt<T, F, Fut>(
        &self,
        tokens: &Arc<dyn TokenClient>,
        op: F,
    ) -> (Result<T, SessionError>, Option<(String, String)>)
    where
        F: Fn(Arc<dyn TokenClient>, String) -> Fut,
        Fut: Future<Output = Result<T, SessionError>>,
    {
        match self {
            Self::Room {
                room,
                jwt,
                room_key,
                config,
            } => {
                let first = op(Arc::clone(tokens), jwt.clone()).await;
                if matches!(first, Err(SessionError::Unauthorized)) {
                    let req = token_request(config, room, room_key.clone());
                    match tokens.fetch_token(req).await {
                        Ok(renewed) => {
                            let second = op(Arc::clone(tokens), renewed.token.clone()).await;
                            (second, Some((room.clone(), renewed.token)))
                        }
                        Err(err) => (Err(err), None),
                    }
                } else {
                    (first, None)
                }
            }
            Self::Lobby { config } => {
                let rooms = match tokens
                    .fetch_rooms(&config.invite_code, &config.room_keys)
                    .await
                {
                    Ok(rooms) => rooms,
                    Err(err) => return (Err(err), None),
                };
                let open_room = rooms.iter().find(|r| !r.needs_password()).map(|r| {
                    let key = room_key_for(&config.room_keys, r.parent.as_deref(), &r.name);
                    (r.name.clone(), key)
                });
                let Some((room, room_key)) = open_room else {
                    return (
                        Err(SessionError::Avatar(
                            "no room available to authenticate avatar request".to_owned(),
                        )),
                        None,
                    );
                };
                let token = match tokens
                    .fetch_token(token_request(config, &room, room_key))
                    .await
                {
                    Ok(token) => token,
                    Err(err) => return (Err(err), None),
                };
                (op(Arc::clone(tokens), token.token).await, None)
            }
        }
    }
}

fn avatar_error(err: SessionError) -> SessionError {
    match err {
        SessionError::Avatar(_) => err,
        SessionError::Unauthorized => SessionError::Avatar("session expired".to_owned()),
        SessionError::RateLimited => {
            SessionError::Avatar("too many avatar updates; try again later".to_owned())
        }
        other => SessionError::Avatar(other.to_string()),
    }
}

fn token_request(config: &SessionConfig, room: &str, room_key: Option<String>) -> TokenRequest {
    TokenRequest {
        client_id: config.client_id.clone(),
        display_name: config.display_name.clone(),
        room: room.to_owned(),
        invite_code: config.invite_code.clone(),
        room_key,
    }
}

impl Session {
    pub(super) fn set_avatar(&mut self, path: PathBuf) {
        self.avatar_op_seq = self.avatar_op_seq.wrapping_add(1);
        let seq = self.avatar_op_seq;
        let auth = AvatarAuth::from_session(self);
        let tokens = Arc::clone(&self.tokens);
        let tx = self.avatar_tx.clone();

        tokio::spawn(async move {
            let result = read_avatar_file(path).await;
            let (result, renewed_jwt) = match result {
                Ok(bytes) => {
                    let (upload, renewed) = auth
                        .run_with_jwt(&tokens, move |tokens, jwt| {
                            let bytes = bytes.clone();
                            async move {
                                tokens
                                    .upload_avatar(&jwt, bytes)
                                    .await
                                    .map(|resp| Some(resp.version))
                            }
                        })
                        .await;
                    (upload, renewed)
                }
                Err(err) => (Err(err), None),
            };
            let _ = tx.send(AvatarTaskEvent::Uploaded {
                seq,
                renewed_jwt,
                result,
            });
        });
    }

    pub(super) fn clear_avatar(&mut self) {
        self.avatar_op_seq = self.avatar_op_seq.wrapping_add(1);
        let seq = self.avatar_op_seq;
        let auth = AvatarAuth::from_session(self);
        let tokens = Arc::clone(&self.tokens);
        let tx = self.avatar_tx.clone();

        tokio::spawn(async move {
            let (result, renewed_jwt) = auth
                .run_with_jwt(&tokens, |tokens, jwt| async move {
                    tokens.delete_avatar(&jwt).await.map(|()| None)
                })
                .await;
            let _ = tx.send(AvatarTaskEvent::Uploaded {
                seq,
                renewed_jwt,
                result,
            });
        });
    }

    pub(super) fn ensure_participant_avatars(&mut self, participants: &[Participant]) {
        for participant in participants {
            if let Some(version) = participant.avatar_version.as_deref() {
                self.queue_avatar_download(&participant.identity, version);
            }
        }
    }

    pub(super) fn queue_avatar_download(&mut self, identity: &str, version: &str) {
        if validate_client_id(identity).is_err() || validate_avatar_version(version).is_err() {
            return;
        }
        let Some(cache_dir) = self.config.avatar_cache_dir.clone() else {
            return;
        };
        let key = (identity.to_owned(), version.to_owned());
        if !self.avatar_downloads.insert(key.clone()) {
            return;
        }
        let auth = AvatarAuth::from_session(self);
        let tokens = Arc::clone(&self.tokens);
        let tx = self.avatar_tx.clone();
        spawn_avatar_download(key.0, key.1, cache_dir, auth, tokens, tx);
    }

    pub(super) async fn on_avatar_task_event(&mut self, event: AvatarTaskEvent) {
        match event {
            AvatarTaskEvent::Uploaded {
                seq,
                renewed_jwt,
                result,
            } => {
                self.apply_renewed_jwt(renewed_jwt);
                if seq != self.avatar_op_seq {
                    return;
                }
                match result {
                    Ok(version) => {
                        self.config.avatar_version.clone_from(&version);
                        self.emit(Event::LocalAvatarChanged {
                            version: version.clone(),
                        });
                        if let Phase::Connected { local_identity, .. } = &self.phase {
                            let identity = local_identity.clone();
                            if let Err(err) =
                                self.backend.set_avatar_attribute(version.as_deref()).await
                            {
                                self.emit(Event::CommandFailed(err));
                            }
                            self.emit(Event::ParticipantAvatarChanged {
                                identity: identity.clone(),
                                version: version.clone(),
                            });
                            if let Some(ver) = version.as_deref() {
                                self.queue_avatar_download(&identity, ver);
                            }
                        } else if let Some(ver) = version.as_deref() {
                            let self_id = self.config.client_id.clone();
                            self.queue_avatar_download(&self_id, ver);
                        }
                    }
                    Err(err) => self.emit(Event::CommandFailed(avatar_error(err))),
                }
            }
            AvatarTaskEvent::Downloaded {
                identity,
                version,
                renewed_jwt,
                result,
            } => {
                self.avatar_downloads
                    .remove(&(identity.clone(), version.clone()));
                self.apply_renewed_jwt(renewed_jwt);
                match result {
                    Ok(Some(path)) => self.emit(Event::AvatarCached {
                        identity,
                        version,
                        path,
                    }),
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(%identity, %version, %err, "avatar download failed");
                    }
                }
            }
        }
    }

    fn apply_renewed_jwt(&mut self, renewed_jwt: Option<(String, String)>) {
        if let Some((target_room, new_jwt)) = renewed_jwt
            && let Phase::Connected { room, jwt, .. } = &mut self.phase
            && *room == target_room
        {
            *jwt = new_jwt;
        }
    }
}

fn spawn_avatar_download(
    identity: String,
    version: String,
    cache_dir: PathBuf,
    auth: AvatarAuth,
    tokens: Arc<dyn TokenClient>,
    tx: UnboundedSender<AvatarTaskEvent>,
) {
    tokio::spawn(async move {
        let cache_path = cache_dir.join(format!("{identity}-{version}.png"));
        let already_cached = {
            let check_path = cache_path.clone();
            tokio::task::spawn_blocking(move || is_non_empty_file(&check_path))
                .await
                .unwrap_or(false)
        };
        if already_cached {
            let _ = tx.send(AvatarTaskEvent::Downloaded {
                identity,
                version,
                renewed_jwt: None,
                result: Ok(Some(cache_path)),
            });
            return;
        }

        let id_arg = identity.clone();
        let ver_arg = version.clone();
        let (download, renewed_jwt) = auth
            .run_with_jwt(&tokens, move |tokens, jwt| {
                let id_arg = id_arg.clone();
                let ver_arg = ver_arg.clone();
                async move { tokens.download_avatar(&jwt, &id_arg, Some(&ver_arg)).await }
            })
            .await;

        let result = match download {
            Ok(Some(bytes)) => {
                if bytes.is_empty() {
                    Ok(None)
                } else {
                    let write_path = cache_path.clone();
                    tokio::task::spawn_blocking(move || write_cached_png(&write_path, &bytes))
                        .await
                        .map_err(|err| SessionError::Internal(err.to_string()))
                        .and_then(|res| res)
                        .map(|()| Some(cache_path))
                }
            }
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        };

        let _ = tx.send(AvatarTaskEvent::Downloaded {
            identity,
            version,
            renewed_jwt,
            result,
        });
    });
}

async fn read_avatar_file(path: PathBuf) -> Result<Vec<u8>, SessionError> {
    tokio::task::spawn_blocking(move || {
        let metadata = std::fs::metadata(&path)
            .map_err(|err| SessionError::Avatar(format!("could not read avatar file: {err}")))?;
        if metadata.len() == 0 {
            return Err(SessionError::Avatar("avatar file is empty".to_owned()));
        }
        if metadata.len() > MAX_AVATAR_UPLOAD_BYTES as u64 {
            return Err(SessionError::Avatar(
                "avatar file exceeds 2 MB limit".to_owned(),
            ));
        }
        let bytes = std::fs::read(&path)
            .map_err(|err| SessionError::Avatar(format!("could not read avatar file: {err}")))?;
        if bytes.is_empty() {
            return Err(SessionError::Avatar("avatar file is empty".to_owned()));
        }
        if bytes.len() > MAX_AVATAR_UPLOAD_BYTES {
            return Err(SessionError::Avatar(
                "avatar file exceeds 2 MB limit".to_owned(),
            ));
        }
        Ok(bytes)
    })
    .await
    .map_err(|err| SessionError::Internal(err.to_string()))?
}

fn is_non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

fn write_cached_png(target: &Path, bytes: &[u8]) -> Result<(), SessionError> {
    let Some(parent) = target.parent() else {
        return Err(SessionError::Internal(
            "avatar cache path has no parent directory".to_owned(),
        ));
    };
    std::fs::create_dir_all(parent)
        .map_err(|err| SessionError::Avatar(format!("could not create avatar cache: {err}")))?;
    let tmp_name = format!("{}.tmp", uuid::Uuid::new_v4());
    let tmp_path = parent.join(tmp_name);
    if let Err(err) = std::fs::write(&tmp_path, bytes) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(SessionError::Avatar(format!(
            "could not write avatar cache: {err}"
        )));
    }
    if let Err(err) = std::fs::rename(&tmp_path, target) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(SessionError::Avatar(format!(
            "could not finalize avatar cache: {err}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use zenha_protocol::{MAX_AVATAR_UPLOAD_BYTES, RoomSummary};
    use tokio::sync::mpsc;

    use crate::api::{Command, Event, Participant, SessionConfig, SessionError};
    use crate::backend::BackendEvent;
    use crate::net::backoff::ReconnectPolicy;
    use crate::handle::EventSink;
    use crate::testing::{BackendCall, FakeBackend, FakeTokenClient};

    use super::super::run;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("zenha-core-avatar-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { path: dir }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct AvatarHarness {
        commands: mpsc::UnboundedSender<Command>,
        events: mpsc::UnboundedReceiver<Event>,
        backend: FakeBackend,
        tokens: FakeTokenClient,
    }

    impl AvatarHarness {
        fn start(cache_dir: PathBuf, initial_version: Option<&str>) -> Self {
            let backend = FakeBackend::new();
            backend.set_local_identity("client-pedro");
            let tokens = FakeTokenClient::new();
            tokens.set_rooms(vec![RoomSummary {
                name: "geral".to_owned(),
                title: "Geral".to_owned(),
                ..RoomSummary::default()
            }]);
            let (command_tx, command_rx) = mpsc::unbounded_channel();
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            let config = SessionConfig {
                token_server_url: "http://unused.invalid".to_owned(),
                client_id: "client-pedro".to_owned(),
                display_name: "pedro".to_owned(),
                invite_code: "secret".to_owned(),
                avatar_version: initial_version.map(str::to_owned),
                avatar_cache_dir: Some(cache_dir),
                room_keys: std::collections::BTreeMap::new(),
            };
            tokio::spawn(run(
                config,
                Arc::new(backend.clone()),
                Arc::new(tokens.clone()),
                command_rx,
                EventSink::new(event_tx),
                ReconnectPolicy::default(),
            ));
            Self {
                commands: command_tx,
                events: event_rx,
                backend,
                tokens,
            }
        }

        async fn join(&mut self, room: &str) -> Vec<Participant> {
            self.commands
                .send(Command::Join {
                    room: room.to_owned(),
                })
                .unwrap();
            assert_eq!(self.events.recv().await.unwrap(), Event::Connecting);
            let participants = match self.events.recv().await.unwrap() {
                Event::Joined { participants, .. } => participants,
                other => panic!("expected Joined, got {other:?}"),
            };
            assert!(matches!(
                self.events.recv().await.unwrap(),
                Event::ChatHistory { before: None, .. }
            ));
            participants
        }
    }

    #[tokio::test]
    async fn avatar_attribute_updates_participant_caches_once_and_ignores_clears() {
        let temp = TempDir::new("attr");
        let cache = temp.path.join("avatars");
        let mut h = AvatarHarness::start(cache.clone(), None);
        h.tokens
            .set_avatar_bytes("client-ana", "a1b2c3d4e5f60708", vec![137, 80, 78, 71]);
        h.join("geral").await;

        h.backend.emit(BackendEvent::AvatarChanged {
            identity: "client-ana".to_owned(),
            version: Some("a1b2c3d4e5f60708".to_owned()),
        });
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::ParticipantAvatarChanged {
                identity: "client-ana".to_owned(),
                version: Some("a1b2c3d4e5f60708".to_owned()),
            }
        );
        let expected_path = cache.join("client-ana-a1b2c3d4e5f60708.png");
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::AvatarCached {
                identity: "client-ana".to_owned(),
                version: "a1b2c3d4e5f60708".to_owned(),
                path: expected_path.clone(),
            }
        );
        assert_eq!(
            std::fs::read(&expected_path).unwrap(),
            vec![137, 80, 78, 71]
        );
        assert_eq!(h.tokens.download_avatar_calls().len(), 1);

        // Re-receiving the same version reads the disk cache instead of hitting HTTP again.
        h.backend.emit(BackendEvent::AvatarChanged {
            identity: "client-ana".to_owned(),
            version: Some("a1b2c3d4e5f60708".to_owned()),
        });
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::ParticipantAvatarChanged {
                identity: "client-ana".to_owned(),
                version: Some("a1b2c3d4e5f60708".to_owned()),
            }
        );
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::AvatarCached {
                identity: "client-ana".to_owned(),
                version: "a1b2c3d4e5f60708".to_owned(),
                path: expected_path,
            }
        );
        assert_eq!(h.tokens.download_avatar_calls().len(), 1);

        // Clearing avatar updates participant with None and skips downloading.
        h.backend.emit(BackendEvent::AvatarChanged {
            identity: "client-ana".to_owned(),
            version: None,
        });
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::ParticipantAvatarChanged {
                identity: "client-ana".to_owned(),
                version: None,
            }
        );
    }

    #[tokio::test]
    async fn join_publishes_initial_avatar_and_set_clear_commands_propagate() {
        let temp = TempDir::new("commands");
        let cache = temp.path.join("avatars");
        let mut h = AvatarHarness::start(cache.clone(), Some("initialver123456"));
        h.backend.set_participants(vec![Participant {
            identity: "client-pedro".to_owned(),
            name: "pedro".to_owned(),
            muted: false,
            speaking: false,
            avatar_version: None,
        }]);
        h.tokens
            .set_avatar_bytes("client-pedro", "initialver123456", vec![1, 2, 3]);

        let joined = h.join("geral").await;
        assert_eq!(
            joined[0].avatar_version.as_deref(),
            Some("initialver123456")
        );
        assert!(
            h.backend
                .calls()
                .contains(&BackendCall::SetAvatarAttribute(Some(
                    "initialver123456".to_owned()
                )))
        );
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::AvatarCached {
                identity: "client-pedro".to_owned(),
                version: "initialver123456".to_owned(),
                path: cache.join("client-pedro-initialver123456.png"),
            }
        );

        let src_file = temp.path.join("photo.png");
        std::fs::write(&src_file, [9, 8, 7, 6]).unwrap();
        h.commands
            .send(Command::SetAvatar { path: src_file })
            .unwrap();
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::LocalAvatarChanged {
                version: Some("0123456789abcdef".to_owned()),
            }
        );
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::ParticipantAvatarChanged {
                identity: "client-pedro".to_owned(),
                version: Some("0123456789abcdef".to_owned()),
            }
        );
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::AvatarCached {
                identity: "client-pedro".to_owned(),
                version: "0123456789abcdef".to_owned(),
                path: cache.join("client-pedro-0123456789abcdef.png"),
            }
        );
        assert!(
            h.backend
                .calls()
                .contains(&BackendCall::SetAvatarAttribute(Some(
                    "0123456789abcdef".to_owned()
                )))
        );

        // Reject oversized local files before hitting token server.
        let huge_file = temp.path.join("huge.png");
        std::fs::write(&huge_file, vec![0u8; MAX_AVATAR_UPLOAD_BYTES + 1]).unwrap();
        h.commands
            .send(Command::SetAvatar { path: huge_file })
            .unwrap();
        assert!(matches!(
            h.events.recv().await.unwrap(),
            Event::CommandFailed(SessionError::Avatar(_))
        ));

        h.commands.send(Command::ClearAvatar).unwrap();
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::LocalAvatarChanged { version: None }
        );
        assert_eq!(
            h.events.recv().await.unwrap(),
            Event::ParticipantAvatarChanged {
                identity: "client-pedro".to_owned(),
                version: None,
            }
        );
        assert_eq!(h.tokens.delete_avatar_calls().len(), 1);
        assert_eq!(
            h.backend.calls().last(),
            Some(&BackendCall::SetAvatarAttribute(None))
        );
    }
}
