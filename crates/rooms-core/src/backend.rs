use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::api::{AudioDevices, FileTransfer, NoiseSuppression, Participant, SessionError};
use crate::capture::CaptureEnd;
use crate::cue::Cue;
use crate::video::{CaptureTarget, FrameSlot, SharePreset};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendEvent {
    ParticipantJoined(Participant),
    ParticipantLeft {
        identity: String,
    },
    MuteChanged {
        identity: String,
        muted: bool,
    },
    SpeakingChanged {
        identity: String,
        speaking: bool,
    },
    DataReceived {
        sender: String,
        topic: String,
        payload: Vec<u8>,
    },
    FileTransfer(FileTransfer),
    RemoteScreenShareStarted {
        identity: String,
    },
    RemoteScreenShareStopped {
        identity: String,
    },
    WatchEnded {
        identity: String,
        reason: Option<String>,
    },
    ScreenShareEnded {
        reason: CaptureEnd,
    },
    MicLevel {
        level_db: i8,
        gate_open: bool,
    },
    Reconnecting,
    Reconnected,
    Disconnected {
        reason: String,
    },
}

#[derive(Debug)]
pub struct ConnectedRoom {
    pub local_identity: String,
    pub participants: Vec<Participant>,
    pub events: UnboundedReceiver<BackendEvent>,
}

#[async_trait]
pub trait MediaBackend: Send + Sync + 'static {
    async fn connect(&self, url: &str, token: &str) -> Result<ConnectedRoom, SessionError>;
    async fn disconnect(&self) -> Result<(), SessionError>;
    async fn set_microphone_muted(&self, muted: bool) -> Result<(), SessionError>;
    async fn set_output_muted(&self, muted: bool) -> Result<(), SessionError>;
    async fn send_data(&self, topic: &str, payload: Vec<u8>) -> Result<(), SessionError>;
    /// Returns as soon as the transfer starts; progress arrives as `BackendEvent::FileTransfer`
    /// with the same id, ending in `Done` (with the source path) or `Failed`.
    async fn send_file(&self, transfer: FileTransfer, path: PathBuf) -> Result<(), SessionError>;
    async fn audio_devices(&self) -> Result<AudioDevices, SessionError>;
    async fn set_input_device(&self, id: Option<String>) -> Result<(), SessionError>;
    async fn set_output_device(&self, id: Option<String>) -> Result<(), SessionError>;
    /// Takes effect at once when in a room, otherwise on the next connection.
    async fn set_noise_suppression(&self, mode: NoiseSuppression) -> Result<(), SessionError>;
    /// Volume, 0 to 100, of the sound of the screen share being watched.
    async fn set_stream_volume(&self, percent: u8) -> Result<(), SessionError>;
    /// Noise gate threshold in dBFS, `None` for off; only used by the enhanced mode.
    async fn set_noise_gate(&self, threshold_db: Option<i8>) -> Result<(), SessionError>;
    async fn capture_targets(&self) -> Result<Vec<CaptureTarget>, SessionError>;
    async fn start_screen_share(
        &self,
        target_id: &str,
        preset: SharePreset,
    ) -> Result<(), SessionError>;
    async fn stop_screen_share(&self) -> Result<(), SessionError>;
    async fn watch_screen_share(
        &self,
        identity: &str,
        frames: FrameSlot,
    ) -> Result<(), SessionError>;
    async fn stop_watching(&self) -> Result<(), SessionError>;
    fn play_cue(&self, cue: Cue);
}
