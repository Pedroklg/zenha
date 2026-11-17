use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::api::{AudioDevice, Command, Event, Participant, SessionConfig};
use crate::backend::BackendEvent;
use crate::backoff::ReconnectPolicy;
use crate::handle::EventSink;
use crate::testing::{FakeBackend, FakeTokenClient};

use super::run;

pub(super) struct Harness {
    pub(super) commands: mpsc::UnboundedSender<Command>,
    pub(super) events: mpsc::UnboundedReceiver<Event>,
    pub(super) backend: FakeBackend,
    pub(super) tokens: FakeTokenClient,
}

pub(super) fn config() -> SessionConfig {
    SessionConfig {
        token_server_url: "http://unused.invalid".to_owned(),
        client_id: "client-pedro".to_owned(),
        display_name: "pedro".to_owned(),
        invite_code: "secret".to_owned(),
        avatar_version: None,
        avatar_cache_dir: None,
        room_keys: BTreeMap::new(),
    }
}

pub(super) fn device(id: &str) -> AudioDevice {
    AudioDevice {
        id: id.to_owned(),
        name: id.to_uppercase(),
    }
}

pub(super) fn participant(identity: &str) -> Participant {
    Participant {
        identity: identity.to_owned(),
        name: identity.to_uppercase(),
        muted: false,
        speaking: false,
        avatar_version: None,
    }
}

pub(super) fn harness() -> Harness {
    harness_with(config())
}

pub(super) fn harness_with(config: SessionConfig) -> Harness {
    let backend = FakeBackend::new();
    let tokens = FakeTokenClient::new();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(
        config,
        Arc::new(backend.clone()),
        Arc::new(tokens.clone()),
        command_rx,
        EventSink::new(event_tx),
        ReconnectPolicy::default(),
    ));
    Harness {
        commands: command_tx,
        events: event_rx,
        backend,
        tokens,
    }
}

impl Harness {
    pub(super) fn send(&self, command: Command) {
        self.commands.send(command).unwrap();
    }

    pub(super) async fn next(&mut self) -> Event {
        self.events.recv().await.unwrap()
    }

    pub(super) async fn join(&mut self, room: &str) {
        self.send(Command::Join {
            room: room.to_owned(),
        });
        assert_eq!(self.next().await, Event::Connecting);
        assert!(matches!(self.next().await, Event::Joined { .. }));
        assert!(matches!(
            self.next().await,
            Event::ChatHistory { before: None, .. }
        ));
    }
}

pub(super) fn data(topic: &str, payload: Vec<u8>) -> BackendEvent {
    BackendEvent::DataReceived {
        sender: Some("ana".to_owned()),
        topic: topic.to_owned(),
        payload,
    }
}
