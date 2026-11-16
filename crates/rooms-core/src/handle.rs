use std::sync::{Arc, PoisonError, RwLock};
use std::thread::JoinHandle;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::api::{Command, Event, SessionConfig, SessionError};
use crate::backend::MediaBackend;
use crate::backoff::ReconnectPolicy;
use crate::session;
use crate::token::{HttpTokenClient, TokenClient, UnavailableTokenClient};

pub type WakeCallback = Box<dyn Fn() + Send + Sync>;
type SharedWake = Arc<RwLock<Option<Arc<dyn Fn() + Send + Sync>>>>;

#[derive(Clone)]
pub(crate) struct EventSink {
    tx: UnboundedSender<Event>,
    wake: SharedWake,
}

impl EventSink {
    pub(crate) fn new(tx: UnboundedSender<Event>) -> Self {
        Self {
            tx,
            wake: Arc::new(RwLock::new(None)),
        }
    }

    pub(crate) fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let wake = self.wake.clone();
        Arc::new(move || {
            let callback = wake.read().unwrap_or_else(PoisonError::into_inner).clone();
            if let Some(callback) = callback {
                callback();
            }
        })
    }

    pub(crate) fn emit(&self, event: Event) {
        if self.tx.send(event).is_err() {
            return;
        }
        let callback = self
            .wake
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(callback) = callback {
            callback();
        }
    }
}

pub struct SessionHandle {
    commands: Option<UnboundedSender<Command>>,
    events: UnboundedReceiver<Event>,
    wake: SharedWake,
    thread: Option<JoinHandle<()>>,
}

impl SessionHandle {
    pub fn spawn(
        config: SessionConfig,
        backend: impl MediaBackend,
        tokens: impl TokenClient,
    ) -> Result<Self, SessionError> {
        Self::spawn_with_policy(config, backend, tokens, ReconnectPolicy::default())
    }

    pub fn spawn_with_http_tokens(
        config: SessionConfig,
        backend: impl MediaBackend,
    ) -> Result<Self, SessionError> {
        let tokens = HttpTokenClient::new(&config.token_server_url)?;
        Self::spawn(config, backend, tokens)
    }

    /// Session that only serves audio device commands, for use before sign-in.
    pub fn spawn_audio_only(backend: impl MediaBackend) -> Result<Self, SessionError> {
        let config = SessionConfig {
            token_server_url: String::new(),
            client_id: String::new(),
            display_name: String::new(),
            invite_code: String::new(),
            avatar_version: None,
            avatar_cache_dir: None,
        };
        Self::spawn(config, backend, UnavailableTokenClient)
    }

    pub fn spawn_with_policy(
        config: SessionConfig,
        backend: impl MediaBackend,
        tokens: impl TokenClient,
        policy: ReconnectPolicy,
    ) -> Result<Self, SessionError> {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let sink = EventSink::new(event_tx);
        let wake = sink.wake.clone();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| SessionError::Internal(err.to_string()))?;
        let backend: Arc<dyn MediaBackend> = Arc::new(backend);
        let tokens: Arc<dyn TokenClient> = Arc::new(tokens);
        let thread = std::thread::Builder::new()
            .name("rooms-core-session".to_owned())
            .spawn(move || {
                runtime.block_on(session::run(
                    config, backend, tokens, command_rx, sink, policy,
                ));
            })
            .map_err(|err| SessionError::Internal(err.to_string()))?;

        Ok(Self {
            commands: Some(command_tx),
            events: event_rx,
            wake,
            thread: Some(thread),
        })
    }

    pub fn send(&self, command: Command) {
        if let Some(commands) = &self.commands
            && commands.send(command).is_err()
        {
            tracing::debug!("session is gone, command dropped");
        }
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn set_wake_callback(&self, callback: WakeCallback) {
        *self.wake.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::from(callback));
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.commands = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("session thread panicked");
        }
    }
}
