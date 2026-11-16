//! Entrada, troca e atualização da lista de salas.

use std::pin::Pin;
use std::time::Duration;

use rooms_protocol::{validate_client_id, validate_display_name, validate_room_name};
use tokio::time::Sleep;

use crate::api::{Event, SessionError};

use super::Session;

pub(super) const ROOMS_CHANGED_DEBOUNCE: Duration = Duration::from_millis(200);

pub(super) async fn wait_optional(sleep: &mut Option<Pin<Box<Sleep>>>) {
    match sleep {
        Some(timer) => timer.as_mut().await,
        None => std::future::pending().await,
    }
}

impl Session {
    pub(super) async fn join(&mut self, room: String) {
        let valid = validate_room_name(&room)
            .and_then(|()| validate_display_name(&self.config.display_name))
            .and_then(|()| validate_client_id(&self.config.client_id));
        if let Err(err) = valid {
            self.emit(Event::CommandFailed(SessionError::InvalidInput(
                err.to_string(),
            )));
            return;
        }
        if self.teardown().await {
            self.emit(Event::Left);
        }
        self.emit(Event::Connecting);
        self.start_attempt(room, None);
    }

    pub(super) async fn set_display_name(&mut self, name: String) {
        if let Err(err) = validate_display_name(&name) {
            self.emit(Event::CommandFailed(SessionError::InvalidInput(
                err.to_string(),
            )));
            return;
        }
        let name = name.trim().to_owned();
        self.config.display_name.clone_from(&name);
        if self.connected()
            && let Err(err) = self.backend.set_display_name(&name).await
        {
            self.emit(Event::CommandFailed(err));
        }
    }

    pub(super) fn refresh_rooms(&self) {
        let tokens = self.tokens.clone();
        let sink = self.sink.clone();
        let invite_code = self.config.invite_code.clone();
        tokio::spawn(async move {
            match tokens.fetch_rooms(&invite_code).await {
                Ok(rooms) => sink.emit(Event::RoomsUpdated(rooms)),
                Err(err) => sink.emit(Event::CommandFailed(err)),
            }
        });
    }

    pub(super) fn schedule_rooms_refresh(&mut self) {
        self.rooms_refresh_sleep = Some(Box::pin(tokio::time::sleep(ROOMS_CHANGED_DEBOUNCE)));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use rooms_protocol::{ROOMS_CHANGED_TOPIC, RoomSummary};
    use tokio::sync::mpsc;

    use crate::api::{Command, Event, SessionError};
    use crate::backend::BackendEvent;
    use crate::backoff::ReconnectPolicy;
    use crate::handle::EventSink;
    use crate::testing::{BackendCall, FakeBackend, FakeTokenClient};

    use super::super::run;
    use super::super::test_support::{config, harness};
    use super::ROOMS_CHANGED_DEBOUNCE;

    #[tokio::test(start_paused = true)]
    async fn invalid_room_name_is_rejected_without_connecting() {
        let mut h = harness();
        h.send(Command::Join {
            room: "Bad Room".to_owned(),
        });
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        assert_eq!(h.backend.connect_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn invalid_client_id_is_rejected_without_connecting() {
        let backend = FakeBackend::new();
        let tokens = FakeTokenClient::new();
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut bad_config = config();
        bad_config.client_id = "Bad_Id".to_owned();
        tokio::spawn(run(
            bad_config,
            Arc::new(backend.clone()),
            Arc::new(tokens),
            command_rx,
            EventSink::new(event_tx),
            ReconnectPolicy::default(),
        ));
        command_tx
            .send(Command::Join {
                room: "geral".to_owned(),
            })
            .unwrap();
        assert!(matches!(
            event_rx.recv().await.unwrap(),
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        assert_eq!(backend.connect_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_rooms_emits_rooms_updated() {
        let mut h = harness();
        let rooms = vec![RoomSummary {
            name: "geral".to_owned(),
            title: String::new(),
            participants: vec!["ana".to_owned()],
        }];
        h.tokens.set_rooms(rooms.clone());
        h.send(Command::RefreshRooms);
        assert_eq!(h.next().await, Event::RoomsUpdated(rooms));
    }

    #[tokio::test(start_paused = true)]
    async fn rooms_changed_debounces_burst_into_single_refresh_preserving_title() {
        let mut h = harness();
        h.tokens.set_rooms(vec![RoomSummary {
            name: "geral".to_owned(),
            title: "Sala Geral".to_owned(),
            participants: vec!["ana".to_owned()],
        }]);
        h.join("geral").await;

        for _ in 0..4 {
            assert!(
                h.backend
                    .emit_server_data(ROOMS_CHANGED_TOPIC, b"{}".to_vec())
            );
            tokio::time::advance(Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(h.tokens.rooms_calls(), 0);

        tokio::time::advance(ROOMS_CHANGED_DEBOUNCE).await;
        let Event::RoomsUpdated(rooms) = h.next().await else {
            panic!("expected RoomsUpdated");
        };
        assert_eq!(h.tokens.rooms_calls(), 1);
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].title, "Sala Geral");
    }

    #[tokio::test(start_paused = true)]
    async fn set_display_name_renames_in_room_and_on_the_next_token() {
        let mut h = harness();
        h.send(Command::SetDisplayName {
            name: "  Ana  ".to_owned(),
        });
        h.join("geral").await;
        assert_eq!(h.tokens.requests()[0].display_name, "Ana");
        assert!(
            !h.backend
                .calls()
                .contains(&BackendCall::SetDisplayName("Ana".to_owned()))
        );

        h.send(Command::SetDisplayName {
            name: "Bia".to_owned(),
        });
        h.send(Command::SetDisplayName {
            name: "   ".to_owned(),
        });
        assert!(matches!(
            h.next().await,
            Event::CommandFailed(SessionError::InvalidInput(_))
        ));
        assert!(
            h.backend
                .calls()
                .contains(&BackendCall::SetDisplayName("Bia".to_owned()))
        );

        h.send(Command::Leave);
        assert_eq!(h.next().await, Event::Left);
        h.join("jogos").await;
        assert_eq!(h.tokens.requests().last().unwrap().display_name, "Bia");
    }

    #[tokio::test(start_paused = true)]
    async fn backend_name_change_is_forwarded() {
        let mut h = harness();
        h.join("geral").await;
        assert!(h.backend.emit(BackendEvent::NameChanged {
            identity: "ana".to_owned(),
            name: "Ana Paula".to_owned(),
        }));
        assert_eq!(
            h.next().await,
            Event::ParticipantNameChanged {
                identity: "ana".to_owned(),
                name: "Ana Paula".to_owned(),
            }
        );
    }
}
