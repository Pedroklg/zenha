use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::api::{AudioDevices, FileTransfer, NoiseSuppression, Participant, SessionError};
use crate::backend::{BackendEvent, ConnectedRoom, MediaBackend};
use crate::audio::cue::Cue;
use crate::video::{CaptureTarget, FrameSlot, SharePreset, VideoFrame};

/// Backend sem mídia: conecta sempre e mostra só o participante local.
///
/// Existe para exercitar o servidor de tokens e a interface enquanto o backend
/// do LiveKit não está implementado. Nada é enviado ao servidor de mídia.
#[derive(Debug)]
pub struct NoMediaBackend {
    identity: String,
    events: Mutex<Option<UnboundedSender<BackendEvent>>>,
}

impl NoMediaBackend {
    pub fn new(identity: &str) -> Self {
        Self {
            identity: identity.trim().to_owned(),
            events: Mutex::new(None),
        }
    }

    fn set_events(&self, events: Option<UnboundedSender<BackendEvent>>) {
        *self.events.lock().unwrap_or_else(PoisonError::into_inner) = events;
    }
}

#[async_trait]
impl MediaBackend for NoMediaBackend {
    async fn connect(&self, _url: &str, _token: &str) -> Result<ConnectedRoom, SessionError> {
        let (tx, rx) = mpsc::unbounded_channel();
        // The sender is kept so the session does not read a closed channel as a disconnect.
        self.set_events(Some(tx));
        Ok(ConnectedRoom {
            local_identity: self.identity.clone(),
            participants: vec![Participant {
                identity: self.identity.clone(),
                name: self.identity.clone(),
                muted: false,
                speaking: false,
                avatar_version: None,
            }],
            events: rx,
        })
    }

    async fn disconnect(&self) -> Result<(), SessionError> {
        self.set_events(None);
        Ok(())
    }

    async fn set_microphone_muted(&self, _muted: bool) -> Result<(), SessionError> {
        Ok(())
    }

    async fn set_output_muted(&self, _muted: bool) -> Result<(), SessionError> {
        Ok(())
    }

    async fn set_display_name(&self, _name: &str) -> Result<(), SessionError> {
        Ok(())
    }

    async fn set_avatar_attribute(&self, _version: Option<&str>) -> Result<(), SessionError> {
        Ok(())
    }

    async fn send_data(&self, _topic: &str, _payload: Vec<u8>) -> Result<(), SessionError> {
        Ok(())
    }

    async fn send_file(&self, _transfer: FileTransfer, _path: PathBuf) -> Result<(), SessionError> {
        Ok(())
    }

    async fn audio_devices(&self) -> Result<AudioDevices, SessionError> {
        Ok(AudioDevices::default())
    }

    async fn set_input_device(&self, _id: Option<String>) -> Result<(), SessionError> {
        Ok(())
    }

    async fn set_output_device(&self, _id: Option<String>) -> Result<(), SessionError> {
        Ok(())
    }

    async fn set_noise_suppression(&self, _mode: NoiseSuppression) -> Result<(), SessionError> {
        Ok(())
    }

    async fn set_stream_volume(&self, _percent: u8) -> Result<(), SessionError> {
        Ok(())
    }

    async fn set_participant_volume(
        &self,
        _identity: &str,
        _percent: u16,
    ) -> Result<(), SessionError> {
        Ok(())
    }

    async fn set_noise_gate(&self, _threshold_db: Option<i8>) -> Result<(), SessionError> {
        Ok(())
    }

    async fn capture_targets(&self) -> Result<Vec<CaptureTarget>, SessionError> {
        Ok(Vec::new())
    }

    async fn capture_thumbnail(
        &self,
        _target: &CaptureTarget,
        _max_side: u32,
        _timeout: Duration,
    ) -> Result<VideoFrame, SessionError> {
        Err(SessionError::ShareUnavailable)
    }

    async fn start_screen_share(
        &self,
        _target_id: &str,
        _preset: SharePreset,
        _preview: FrameSlot,
    ) -> Result<(), SessionError> {
        Err(SessionError::ShareUnavailable)
    }

    async fn stop_screen_share(&self) -> Result<(), SessionError> {
        Ok(())
    }

    async fn watch_screen_share(
        &self,
        _identity: &str,
        _frames: FrameSlot,
    ) -> Result<(), SessionError> {
        Err(SessionError::ShareUnavailable)
    }

    async fn stop_watching(&self) -> Result<(), SessionError> {
        Ok(())
    }

    fn set_ui_visible(&self, _visible: bool) {}

    fn play_cue(&self, _cue: Cue) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_lists_only_the_local_participant_and_keeps_the_channel_open() {
        let backend = NoMediaBackend::new(" Pedro ");
        let mut room = backend.connect("ws://x", "t").await.unwrap();
        assert_eq!(room.local_identity, "Pedro");
        assert_eq!(room.participants.len(), 1);
        assert!(room.events.try_recv().is_err());
        assert!(!room.events.is_closed());
        backend.disconnect().await.unwrap();
        assert!(room.events.recv().await.is_none());
    }

    #[tokio::test]
    async fn screen_sharing_is_unavailable_with_a_clear_error() {
        let backend = NoMediaBackend::new("p");
        assert!(backend.capture_targets().await.unwrap().is_empty());
        assert_eq!(
            backend
                .start_screen_share("x", SharePreset::P720F30, FrameSlot::new())
                .await,
            Err(SessionError::ShareUnavailable)
        );
        assert_eq!(
            backend.watch_screen_share("a", FrameSlot::new()).await,
            Err(SessionError::ShareUnavailable)
        );
    }
}
