//! `FakeBackend`: fake media backend that records the calls it receives.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::Instant;

use crate::api::{
    AudioDevice, AudioDevices, FileTransfer, NoiseSuppression, Participant, SessionError,
};
use crate::audio::cue::Cue;
use crate::audio::participant_volume::ParticipantVolumes;
use crate::backend::{BackendEvent, ConnectedRoom, MediaBackend};
use crate::video::{CaptureTarget, FrameSlot, SharePreset, VideoFrame};

use super::lock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCall {
    Connect {
        url: String,
        token: String,
    },
    Disconnect,
    SetMicrophoneMuted(bool),
    SetOutputMuted(bool),
    SetDisplayName(String),
    SetAvatarAttribute(Option<String>),
    SendData {
        topic: String,
        payload: Vec<u8>,
    },
    SendFile {
        transfer: FileTransfer,
        path: PathBuf,
    },
    ListAudioDevices,
    SetInputDevice(Option<String>),
    SetOutputDevice(Option<String>),
    SetNoiseSuppression(NoiseSuppression),
    SetStreamVolume(u8),
    SetParticipantVolume {
        identity: String,
        percent: u16,
    },
    SetNoiseGate(Option<i8>),
    ListCaptureTargets,
    StartScreenShare {
        target_id: String,
        preset: SharePreset,
    },
    StopScreenShare,
    WatchScreenShare {
        identity: String,
    },
    StopWatching,
    SetUiVisible(bool),
    PlayCue(Cue),
}

#[derive(Debug, Clone)]
pub enum FakeThumbnail {
    Ready(VideoFrame),
    Delay { delay: Duration, frame: VideoFrame },
    Fail(SessionError),
}

#[derive(Debug)]
struct BackendState {
    calls: Vec<BackendCall>,
    connect_instants: Vec<Instant>,
    remaining_connect_failures: u32,
    local_identity: String,
    participants: Vec<Participant>,
    events: Option<UnboundedSender<BackendEvent>>,
    inputs: Vec<AudioDevice>,
    outputs: Vec<AudioDevice>,
    selected_input: Option<String>,
    selected_output: Option<String>,
    applied_devices: Vec<(Option<String>, Option<String>)>,
    device_failure: Option<SessionError>,
    file_failure: Option<SessionError>,
    targets: Vec<CaptureTarget>,
    thumbnails: HashMap<String, FakeThumbnail>,
    share_failure: Option<SessionError>,
    sharing: Option<(String, SharePreset, FrameSlot)>,
    watching: Option<(String, FrameSlot)>,
    participant_volumes: ParticipantVolumes,
    ui_hidden: bool,
}

#[derive(Debug, Clone)]
pub struct FakeBackend {
    state: Arc<Mutex<BackendState>>,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBackend {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(BackendState {
                calls: Vec::new(),
                connect_instants: Vec::new(),
                remaining_connect_failures: 0,
                local_identity: "local".to_owned(),
                participants: Vec::new(),
                events: None,
                inputs: Vec::new(),
                outputs: Vec::new(),
                selected_input: None,
                selected_output: None,
                applied_devices: Vec::new(),
                device_failure: None,
                file_failure: None,
                targets: Vec::new(),
                thumbnails: HashMap::new(),
                share_failure: None,
                sharing: None,
                watching: None,
                participant_volumes: ParticipantVolumes::default(),
                ui_hidden: false,
            })),
        }
    }

    pub fn set_local_identity(&self, identity: &str) {
        lock(&self.state).local_identity = identity.to_owned();
    }

    pub fn set_participants(&self, participants: Vec<Participant>) {
        lock(&self.state).participants = participants;
    }

    pub fn fail_next_connects(&self, count: u32) {
        lock(&self.state).remaining_connect_failures = count;
    }

    pub fn set_audio_devices(&self, inputs: Vec<AudioDevice>, outputs: Vec<AudioDevice>) {
        let mut state = lock(&self.state);
        state.inputs = inputs;
        state.outputs = outputs;
    }

    pub fn fail_next_device_change(&self, error: SessionError) {
        lock(&self.state).device_failure = Some(error);
    }

    pub fn fail_next_send_file(&self, error: SessionError) {
        lock(&self.state).file_failure = Some(error);
    }

    /// Per successful connect: the input and output actually in use, with an
    /// unknown remembered id falling back to the system default (`None`).
    pub fn applied_devices(&self) -> Vec<(Option<String>, Option<String>)> {
        lock(&self.state).applied_devices.clone()
    }

    pub fn set_capture_targets(&self, targets: Vec<CaptureTarget>) {
        lock(&self.state).targets = targets;
    }

    pub fn set_thumbnail(&self, id: &str, behavior: FakeThumbnail) {
        lock(&self.state).thumbnails.insert(id.to_owned(), behavior);
    }

    pub fn clear_thumbnails(&self) {
        lock(&self.state).thumbnails.clear();
    }

    pub fn fail_next_share(&self, error: SessionError) {
        lock(&self.state).share_failure = Some(error);
    }

    pub fn sharing(&self) -> Option<(String, SharePreset)> {
        lock(&self.state)
            .sharing
            .as_ref()
            .map(|(id, preset, _)| (id.clone(), *preset))
    }

    pub fn push_share_preview_frame(&self, frame: VideoFrame) -> bool {
        let state = lock(&self.state);
        if state.ui_hidden {
            return false;
        }
        match &state.sharing {
            Some((_, _, slot)) => {
                slot.push(frame);
                true
            }
            None => false,
        }
    }

    pub fn watching(&self) -> Option<String> {
        lock(&self.state)
            .watching
            .as_ref()
            .map(|(id, _)| id.clone())
    }

    /// Pushes a synthetic frame into the slot of the current watch; false when not watching.
    pub fn push_frame(&self, frame: VideoFrame) -> bool {
        let state = lock(&self.state);
        if state.ui_hidden {
            return false;
        }
        match &state.watching {
            Some((_, slot)) => {
                slot.push(frame);
                true
            }
            None => false,
        }
    }

    /// Returns false when no connection is open to receive the event.
    pub fn emit(&self, event: BackendEvent) -> bool {
        match &lock(&self.state).events {
            Some(tx) => tx.send(event).is_ok(),
            None => false,
        }
    }

    pub fn emit_server_data(&self, topic: &str, payload: Vec<u8>) -> bool {
        self.emit(BackendEvent::DataReceived {
            sender: None,
            topic: topic.to_owned(),
            payload,
        })
    }

    pub fn calls(&self) -> Vec<BackendCall> {
        lock(&self.state).calls.clone()
    }

    pub fn participant_volume_gain(&self, identity: &str) -> Option<f64> {
        lock(&self.state).participant_volumes.gain_for(identity)
    }

    pub fn connect_count(&self) -> usize {
        lock(&self.state).connect_instants.len()
    }

    pub fn connect_instants(&self) -> Vec<Instant> {
        lock(&self.state).connect_instants.clone()
    }

    pub fn is_connected(&self) -> bool {
        lock(&self.state).events.is_some()
    }
}

fn known_device(devices: &[AudioDevice], id: Option<&str>) -> Option<String> {
    id.filter(|id| devices.iter().any(|device| device.id == *id))
        .map(str::to_owned)
}

#[async_trait]
impl MediaBackend for FakeBackend {
    async fn connect(&self, url: &str, token: &str) -> Result<ConnectedRoom, SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::Connect {
            url: url.to_owned(),
            token: token.to_owned(),
        });
        state.connect_instants.push(Instant::now());
        if state.remaining_connect_failures > 0 {
            state.remaining_connect_failures -= 1;
            return Err(SessionError::Connection("fake connect failure".to_owned()));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        state.events = Some(tx);
        let input = known_device(&state.inputs, state.selected_input.as_deref());
        let output = known_device(&state.outputs, state.selected_output.as_deref());
        state.applied_devices.push((input, output));
        Ok(ConnectedRoom {
            local_identity: state.local_identity.clone(),
            participants: state.participants.clone(),
            events: rx,
        })
    }

    async fn disconnect(&self) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::Disconnect);
        state.events = None;
        state.sharing = None;
        state.watching = None;
        Ok(())
    }

    async fn set_microphone_muted(&self, muted: bool) -> Result<(), SessionError> {
        lock(&self.state)
            .calls
            .push(BackendCall::SetMicrophoneMuted(muted));
        Ok(())
    }

    async fn set_output_muted(&self, muted: bool) -> Result<(), SessionError> {
        lock(&self.state)
            .calls
            .push(BackendCall::SetOutputMuted(muted));
        Ok(())
    }

    async fn set_display_name(&self, name: &str) -> Result<(), SessionError> {
        lock(&self.state)
            .calls
            .push(BackendCall::SetDisplayName(name.to_owned()));
        Ok(())
    }

    async fn set_avatar_attribute(&self, version: Option<&str>) -> Result<(), SessionError> {
        lock(&self.state)
            .calls
            .push(BackendCall::SetAvatarAttribute(version.map(str::to_owned)));
        Ok(())
    }

    async fn send_data(&self, topic: &str, payload: Vec<u8>) -> Result<(), SessionError> {
        lock(&self.state).calls.push(BackendCall::SendData {
            topic: topic.to_owned(),
            payload,
        });
        Ok(())
    }

    async fn send_file(&self, transfer: FileTransfer, path: PathBuf) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::SendFile { transfer, path });
        match state.file_failure.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn audio_devices(&self) -> Result<AudioDevices, SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::ListAudioDevices);
        Ok(AudioDevices {
            inputs: state.inputs.clone(),
            outputs: state.outputs.clone(),
            selected_input: state.selected_input.clone(),
            selected_output: state.selected_output.clone(),
        })
    }

    async fn set_input_device(&self, id: Option<String>) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::SetInputDevice(id.clone()));
        if let Some(error) = state.device_failure.take() {
            return Err(error);
        }
        state.selected_input = id;
        Ok(())
    }

    async fn set_output_device(&self, id: Option<String>) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::SetOutputDevice(id.clone()));
        if let Some(error) = state.device_failure.take() {
            return Err(error);
        }
        state.selected_output = id;
        Ok(())
    }

    async fn set_noise_suppression(&self, mode: NoiseSuppression) -> Result<(), SessionError> {
        lock(&self.state)
            .calls
            .push(BackendCall::SetNoiseSuppression(mode));
        Ok(())
    }

    async fn set_stream_volume(&self, percent: u8) -> Result<(), SessionError> {
        lock(&self.state)
            .calls
            .push(BackendCall::SetStreamVolume(percent));
        Ok(())
    }

    async fn set_participant_volume(
        &self,
        identity: &str,
        percent: u16,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::SetParticipantVolume {
            identity: identity.to_owned(),
            percent,
        });
        state.participant_volumes.set(identity, percent);
        Ok(())
    }

    async fn set_noise_gate(&self, threshold_db: Option<i8>) -> Result<(), SessionError> {
        lock(&self.state)
            .calls
            .push(BackendCall::SetNoiseGate(threshold_db));
        Ok(())
    }

    async fn capture_targets(&self) -> Result<Vec<CaptureTarget>, SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::ListCaptureTargets);
        Ok(state.targets.clone())
    }

    async fn capture_thumbnail(
        &self,
        target: &CaptureTarget,
        _max_side: u32,
        _timeout: Duration,
    ) -> Result<VideoFrame, SessionError> {
        let behavior = lock(&self.state).thumbnails.get(&target.id).cloned();
        match behavior {
            Some(FakeThumbnail::Ready(frame)) => Ok(frame),
            Some(FakeThumbnail::Delay { delay, frame }) => {
                tokio::time::sleep(delay).await;
                Ok(frame)
            }
            Some(FakeThumbnail::Fail(err)) => Err(err),
            None => Err(SessionError::TargetNotFound(target.id.clone())),
        }
    }

    async fn start_screen_share(
        &self,
        target_id: &str,
        preset: SharePreset,
        preview: FrameSlot,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::StartScreenShare {
            target_id: target_id.to_owned(),
            preset,
        });
        if let Some(error) = state.share_failure.take() {
            return Err(error);
        }
        if state.sharing.is_some() {
            return Err(SessionError::ShareActive);
        }
        state.sharing = Some((target_id.to_owned(), preset, preview));
        Ok(())
    }

    async fn stop_screen_share(&self) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::StopScreenShare);
        state.sharing = None;
        Ok(())
    }

    async fn watch_screen_share(
        &self,
        identity: &str,
        frames: FrameSlot,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::WatchScreenShare {
            identity: identity.to_owned(),
        });
        state.watching = Some((identity.to_owned(), frames));
        Ok(())
    }

    async fn stop_watching(&self) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::StopWatching);
        state.watching = None;
        Ok(())
    }

    fn set_ui_visible(&self, visible: bool) {
        let mut state = lock(&self.state);
        state.calls.push(BackendCall::SetUiVisible(visible));
        state.ui_hidden = !visible;
    }

    fn play_cue(&self, cue: Cue) {
        lock(&self.state).calls.push(BackendCall::PlayCue(cue));
    }
}
