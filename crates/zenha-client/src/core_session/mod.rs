//! Bridge between the UI and `zenha-core`: `CoreSession` implements `SessionPort`.

mod commands;
mod errors;
mod feed;
#[cfg(test)]
mod tests;
mod translate;

use std::collections::VecDeque;
use std::sync::Arc;

#[cfg(windows)]
use zenha_core::LiveKitBackend;
#[cfg(not(windows))]
use zenha_core::NoMediaBackend;
use zenha_core::{SessionConfig, SessionHandle};

use crate::ui_model::{Credentials, SessionPort, UiCommand, UiEvent};

use commands::to_core;
use errors::describe;
use translate::Translator;

pub use feed::TestFeed;

pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct CoreSession {
    handle: Option<SessionHandle>,
    translator: Translator,
    queued: VecDeque<UiEvent>,
    wake: Wake,
}

#[cfg(windows)]
fn media_backend(_display_name: &str) -> LiveKitBackend {
    LiveKitBackend::new()
}

#[cfg(not(windows))]
fn media_backend(display_name: &str) -> NoMediaBackend {
    NoMediaBackend::new(display_name)
}

impl CoreSession {
    /// Starts with a session that only serves audio device commands, so the
    /// settings panel works on the login screen; `configure` replaces it.
    pub fn new(wake: Wake) -> Self {
        let mut session = Self {
            handle: None,
            translator: Translator::default(),
            queued: VecDeque::new(),
            wake,
        };
        match SessionHandle::spawn_audio_only(media_backend("")) {
            Ok(handle) => session.install(handle),
            Err(err) => tracing::warn!(%err, "could not start the audio device session"),
        }
        session
    }

    fn install(&mut self, handle: SessionHandle) {
        let wake = Arc::clone(&self.wake);
        handle.set_wake_callback(Box::new(move || wake()));
        self.handle = Some(handle);
    }
}

impl SessionPort for CoreSession {
    fn configure(&mut self, credentials: Credentials) {
        self.handle = None;
        self.translator = Translator::default();
        self.queued.clear();

        let backend = media_backend(&credentials.client_id);
        let config = SessionConfig {
            token_server_url: credentials.token_server_url,
            client_id: credentials.client_id,
            display_name: credentials.display_name,
            invite_code: credentials.invite_code,
            avatar_version: credentials.avatar_version,
            avatar_cache_dir: credentials.avatar_cache_dir,
            room_keys: credentials.room_keys,
        };
        match SessionHandle::spawn_with_http_tokens(config, backend) {
            Ok(handle) => self.install(handle),
            Err(err) => self.queued.push_back(UiEvent::Failed {
                message: describe(&err),
            }),
        }
    }

    fn send(&self, command: UiCommand) {
        if let Some(handle) = &self.handle {
            if matches!(command, UiCommand::Join { .. }) && self.translator.room.is_some() {
                self.translator.switch_pending.set(true);
            }
            handle.send(to_core(command));
        }
    }

    fn try_recv(&mut self) -> Option<UiEvent> {
        loop {
            if let Some(event) = self.queued.pop_front() {
                return Some(event);
            }
            let event = self.handle.as_mut()?.try_recv()?;
            self.queued.extend(self.translator.translate(event));
        }
    }
}
