//! Máquina de estados de conexão, teardown e reconexão com backoff.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Sleep;
use zenha_protocol::TokenRequest;

use crate::api::{Event, SessionError};
use crate::backend::{BackendEvent, ConnectedRoom};

use super::Session;

pub(super) struct AttemptConnected {
    pub(super) jwt: String,
    pub(super) room: ConnectedRoom,
}

pub(super) type Attempt =
    Pin<Box<dyn Future<Output = Result<AttemptConnected, SessionError>> + Send>>;

pub(super) enum Phase {
    Idle,
    Connecting {
        room: String,
        retry: Option<u32>,
        attempt: Attempt,
    },
    Connected {
        room: String,
        jwt: String,
        local_identity: String,
        events: UnboundedReceiver<BackendEvent>,
        reconnecting: bool,
    },
    Backoff {
        room: String,
        attempt: u32,
        sleep: Pin<Box<Sleep>>,
    },
}

pub(super) enum Input {
    Backend(Option<BackendEvent>),
    Attempt(Result<AttemptConnected, SessionError>),
    BackoffElapsed,
}

impl Phase {
    pub(super) async fn wait(&mut self) -> Input {
        match self {
            Phase::Idle => std::future::pending().await,
            Phase::Connecting { attempt, .. } => Input::Attempt(attempt.as_mut().await),
            Phase::Connected { events, .. } => Input::Backend(events.recv().await),
            Phase::Backoff { sleep, .. } => {
                sleep.as_mut().await;
                Input::BackoffElapsed
            }
        }
    }
}

impl Session {
    pub(super) fn token_request(&self, room: &str) -> TokenRequest {
        TokenRequest {
            client_id: self.config.client_id.clone(),
            display_name: self.config.display_name.clone(),
            room: room.to_owned(),
            invite_code: self.config.invite_code.clone(),
            room_key: self.room_key(room),
        }
    }

    pub(super) fn start_attempt(&mut self, room: String, retry: Option<u32>) {
        let request = self.token_request(&room);
        let tokens = self.tokens.clone();
        let backend = self.backend.clone();
        let attempt: Attempt = Box::pin(async move {
            let token = tokens.fetch_token(request).await?;
            let connected = backend.connect(&token.livekit_url, &token.token).await?;
            Ok(AttemptConnected {
                jwt: token.token,
                room: connected,
            })
        });
        self.phase = Phase::Connecting {
            room,
            retry,
            attempt,
        };
    }

    pub(super) async fn teardown(&mut self) -> bool {
        self.capture_targets_seq = self.capture_targets_seq.wrapping_add(1);
        self.thumbnail_job = None;
        self.reset_video().await;
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => false,
            Phase::Connected { .. } => {
                if let Err(err) = self.backend.disconnect().await {
                    tracing::warn!(%err, "backend disconnect failed");
                }
                true
            }
            Phase::Connecting { .. } | Phase::Backoff { .. } => true,
        }
    }

    pub(super) async fn on_input(&mut self, input: Input) {
        match input {
            Input::Attempt(result) => self.on_attempt(result).await,
            Input::Backend(event) => self.on_backend(event).await,
            Input::BackoffElapsed => {
                if let Phase::Backoff { room, attempt, .. } =
                    std::mem::replace(&mut self.phase, Phase::Idle)
                {
                    self.start_attempt(room, Some(attempt));
                }
            }
        }
    }

    async fn on_attempt(&mut self, result: Result<AttemptConnected, SessionError>) {
        let Phase::Connecting { room, retry, .. } = std::mem::replace(&mut self.phase, Phase::Idle)
        else {
            return;
        };
        match result {
            Ok(connected) => self.on_connected(room, retry, connected).await,
            Err(SessionError::RoomLocked(locked)) => {
                self.forget_room_key(&locked);
                self.emit(Event::Failed(SessionError::RoomLocked(locked)));
            }
            Err(err) => match retry {
                Some(attempt)
                    if attempt < self.policy.max_attempts
                        && err != SessionError::InviteRejected =>
                {
                    self.schedule_retry(room, attempt + 1);
                }
                _ => self.emit(Event::Failed(err)),
            },
        }
    }

    async fn on_connected(
        &mut self,
        room: String,
        retry: Option<u32>,
        connected: AttemptConnected,
    ) {
        let AttemptConnected {
            jwt,
            room:
                ConnectedRoom {
                    local_identity,
                    mut participants,
                    events,
                },
        } = connected;
        if let Err(err) = self.backend.set_microphone_muted(self.muted).await {
            tracing::warn!(%err, "could not apply microphone state");
        }
        if let Err(err) = self.backend.set_output_muted(self.deafened).await {
            tracing::warn!(%err, "could not apply output state");
        }
        if let Some(version) = self.config.avatar_version.clone() {
            if let Err(err) = self.backend.set_avatar_attribute(Some(&version)).await {
                tracing::warn!(%err, "could not publish initial avatar attribute");
            }
            for participant in &mut participants {
                if participant.identity == local_identity {
                    participant.avatar_version = Some(version.clone());
                }
            }
        }
        let volumes: Vec<(String, u16)> = self
            .participant_volumes
            .iter()
            .map(|(identity, percent)| (identity.to_owned(), percent))
            .collect();
        for (identity, percent) in volumes {
            if let Err(err) = self
                .backend
                .set_participant_volume(&identity, percent)
                .await
            {
                tracing::warn!(%err, identity, "could not apply participant volume");
            }
        }
        self.phase = Phase::Connected {
            room: room.clone(),
            jwt,
            local_identity: local_identity.clone(),
            events,
            reconnecting: false,
        };
        let avatar_targets = participants.clone();
        if retry.is_some() {
            self.emit(Event::Reconnected);
            self.emit(Event::ParticipantList(participants));
        } else {
            self.emit(Event::Joined {
                room,
                local_identity,
                participants,
            });
            match self.fetch_history_page(None).await {
                Ok((messages, has_more)) => self.emit(Event::ChatHistory {
                    messages,
                    has_more,
                    before: None,
                }),
                Err(err) => {
                    tracing::warn!(%err, "could not load initial chat history");
                }
            }
        }
        self.ensure_participant_avatars(&avatar_targets);
    }

    fn schedule_retry(&mut self, room: String, attempt: u32) {
        let sleep = Box::pin(tokio::time::sleep(self.policy.delay(attempt)));
        self.phase = Phase::Backoff {
            room,
            attempt,
            sleep,
        };
    }

    pub(super) async fn on_unexpected_disconnect(&mut self, reason: &str) {
        tracing::warn!(reason, "unexpected disconnect, starting reconnect");
        self.reset_video().await;
        if let Phase::Connected {
            room, reconnecting, ..
        } = std::mem::replace(&mut self.phase, Phase::Idle)
        {
            if !reconnecting {
                self.emit(Event::Reconnecting);
            }
            self.schedule_retry(room, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::Instant;

    use crate::api::{Command, Event, RemovalReason, SessionError};
    use crate::backend::BackendEvent;
    use crate::net::backoff::ReconnectPolicy;
    use crate::video::{CaptureKind, CaptureTarget, SharePreset};

    use super::super::test_support::{harness, participant};

    #[tokio::test(start_paused = true)]
    async fn join_emits_connecting_then_joined_and_forwards_participant_changes() {
        let mut h = harness();
        h.backend.set_participants(vec![participant("a")]);
        h.send(Command::Join {
            room: "geral".to_owned(),
        });
        assert_eq!(h.next().await, Event::Connecting);
        assert_eq!(
            h.next().await,
            Event::Joined {
                room: "geral".to_owned(),
                local_identity: "local".to_owned(),
                participants: vec![participant("a")],
            }
        );
        assert_eq!(
            h.next().await,
            Event::ChatHistory {
                messages: Vec::new(),
                has_more: false,
                before: None,
            }
        );
        assert_eq!(h.tokens.requests()[0].client_id, "client-pedro");
        assert_eq!(h.tokens.requests()[0].invite_code, "secret");

        h.backend
            .emit(BackendEvent::ParticipantJoined(participant("b")));
        h.backend.emit(BackendEvent::MuteChanged {
            identity: "b".to_owned(),
            muted: true,
        });
        h.backend.emit(BackendEvent::SpeakingChanged {
            identity: "b".to_owned(),
            speaking: true,
        });
        h.backend.emit(BackendEvent::ParticipantLeft {
            identity: "a".to_owned(),
        });
        assert_eq!(h.next().await, Event::ParticipantJoined(participant("b")));
        assert_eq!(
            h.next().await,
            Event::ParticipantMuteChanged {
                identity: "b".to_owned(),
                muted: true
            }
        );
        assert_eq!(
            h.next().await,
            Event::ParticipantSpeakingChanged {
                identity: "b".to_owned(),
                speaking: true
            }
        );
        assert_eq!(
            h.next().await,
            Event::ParticipantLeft {
                identity: "a".to_owned()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn invite_rejected_fails_and_later_join_works() {
        let mut h = harness();
        h.tokens.set_reject_invite(true);
        h.send(Command::Join {
            room: "geral".to_owned(),
        });
        assert_eq!(h.next().await, Event::Connecting);
        assert_eq!(h.next().await, Event::Failed(SessionError::InviteRejected));
        assert!(!h.backend.is_connected());

        h.tokens.set_reject_invite(false);
        h.join("geral").await;
        assert!(h.backend.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn unexpected_disconnect_reconnects_with_backoff() {
        let mut h = harness();
        h.join("geral").await;
        h.send(Command::SetMuted(true));
        h.backend.fail_next_connects(2);
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert!(matches!(h.next().await, Event::ParticipantList(_)));

        let instants = h.backend.connect_instants();
        let gaps: Vec<Duration> = instants.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(
            gaps,
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4)
            ]
        );
        assert_eq!(h.tokens.requests().len(), 4);
        assert!(h.backend.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn exhausting_attempts_ends_in_failed() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.fail_next_connects(100);
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        assert!(matches!(
            h.next().await,
            Event::Failed(SessionError::Connection(_))
        ));

        let policy = ReconnectPolicy::default();
        let instants = h.backend.connect_instants();
        assert_eq!(instants.len(), 1 + policy.max_attempts as usize);
        let gaps: Vec<Duration> = instants.windows(2).map(|w| w[1] - w[0]).collect();
        let expected: Vec<Duration> = (1..=policy.max_attempts).map(|n| policy.delay(n)).collect();
        assert_eq!(gaps, expected);

        h.backend.fail_next_connects(0);
        h.join("geral").await;
    }

    #[tokio::test(start_paused = true)]
    async fn leave_during_backoff_stops_retries() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        h.send(Command::Leave);
        assert_eq!(h.next().await, Event::Left);

        let start = Instant::now();
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert!(Instant::now() - start >= Duration::from_secs(120));
        assert_eq!(h.backend.connect_count(), 1);
        assert!(h.events.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn backend_reconnect_events_are_forwarded_once() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.emit(BackendEvent::Reconnecting);
        h.backend.emit(BackendEvent::Reconnecting);
        h.backend.emit(BackendEvent::Reconnected);
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert!(h.events.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn removed_from_room_leaves_without_auto_reconnect() {
        let mut h = harness();
        h.backend.set_participants(vec![participant("ana")]);
        h.join("geral").await;
        h.backend.set_capture_targets(vec![CaptureTarget {
            id: "screen-1".to_owned(),
            name: "Main".to_owned(),
            kind: CaptureKind::Screen,
        }]);
        h.send(Command::StartScreenShare {
            target_id: "screen-1".to_owned(),
            preset: SharePreset::P720F30,
        });
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
        h.send(Command::WatchScreenShare {
            identity: "ana".to_owned(),
        });
        assert!(matches!(h.next().await, Event::WatchingStarted { .. }));

        assert!(h.backend.emit(BackendEvent::RemovedFromRoom {
            reason: RemovalReason::Kicked,
        }));
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(
            h.next().await,
            Event::RemovedFromRoom {
                reason: RemovalReason::Kicked,
            }
        );
        assert!(h.backend.sharing().is_none());
        assert_eq!(h.backend.watching(), None);

        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(h.backend.connect_count(), 1);

        // RoomDeleted follows the same no-reconnect path.
        h.join("jogos").await;
        assert!(h.backend.emit(BackendEvent::RemovedFromRoom {
            reason: RemovalReason::RoomDeleted,
        }));
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(
            h.next().await,
            Event::RemovedFromRoom {
                reason: RemovalReason::RoomDeleted,
            }
        );
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(h.backend.connect_count(), 2);
    }
}
