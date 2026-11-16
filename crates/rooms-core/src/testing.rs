use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use rooms_protocol::{
    AdminLoginRequest, AdminLoginResponse, AdminRoom, ChatHistoryResponse, ChatMessage,
    CreateRoomRequest, MoveParticipantRequest, RoomSummary, SaveChatRequest, TokenRequest,
    TokenResponse, UpdateInviteRequest, UpdateRoomRequest,
};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::Instant;

use crate::api::{
    AudioDevice, AudioDevices, FileTransfer, NoiseSuppression, Participant, SessionError,
};
use crate::backend::{BackendEvent, ConnectedRoom, MediaBackend};
use crate::cue::Cue;
use crate::participant_volume::ParticipantVolumes;
use crate::token::TokenClient;
use crate::video::{CaptureTarget, FrameSlot, SharePreset, VideoFrame};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminTokenCall {
    Login(AdminLoginRequest),
    CreateRoom {
        jwt: String,
        request: CreateRoomRequest,
    },
    UpdateRoom {
        jwt: String,
        id: String,
        request: UpdateRoomRequest,
    },
    DeleteRoom {
        jwt: String,
        id: String,
    },
    MuteParticipant {
        jwt: String,
        room: String,
        identity: String,
    },
    StopShareParticipant {
        jwt: String,
        room: String,
        identity: String,
    },
    KickParticipant {
        jwt: String,
        room: String,
        identity: String,
    },
    MoveParticipant {
        jwt: String,
        room: String,
        identity: String,
        request: MoveParticipantRequest,
    },
    DeleteChat {
        jwt: String,
        room: String,
        id: String,
    },
    RotateInvite {
        jwt: String,
        request: UpdateInviteRequest,
    },
}

#[derive(Debug)]
struct TokenState {
    requests: Vec<TokenRequest>,
    reject_invite: bool,
    remaining_failures: u32,
    rooms: Vec<RoomSummary>,
    rooms_calls: usize,
    chat_history_pages: HashMap<Option<String>, ChatHistoryResponse>,
    save_calls: Vec<(String, String, SaveChatRequest)>,
    history_calls: Vec<(String, String, Option<String>)>,
    delete_calls: Vec<(String, String, String)>,
    save_results: VecDeque<Result<ChatMessage, SessionError>>,
    history_results: VecDeque<Result<ChatHistoryResponse, SessionError>>,
    delete_results: VecDeque<Result<(), SessionError>>,
    admin_calls: Vec<AdminTokenCall>,
    admin_login_results: VecDeque<Result<AdminLoginResponse, SessionError>>,
    admin_room_results: VecDeque<Result<AdminRoom, SessionError>>,
    admin_unit_results: VecDeque<Result<(), SessionError>>,
}

#[derive(Debug, Clone)]
pub struct FakeTokenClient {
    state: Arc<Mutex<TokenState>>,
}

impl Default for FakeTokenClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeTokenClient {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(TokenState {
                requests: Vec::new(),
                reject_invite: false,
                remaining_failures: 0,
                rooms: Vec::new(),
                rooms_calls: 0,
                chat_history_pages: HashMap::new(),
                save_calls: Vec::new(),
                history_calls: Vec::new(),
                delete_calls: Vec::new(),
                save_results: VecDeque::new(),
                history_results: VecDeque::new(),
                delete_results: VecDeque::new(),
                admin_calls: Vec::new(),
                admin_login_results: VecDeque::new(),
                admin_room_results: VecDeque::new(),
                admin_unit_results: VecDeque::new(),
            })),
        }
    }

    pub fn set_reject_invite(&self, reject: bool) {
        lock(&self.state).reject_invite = reject;
    }

    pub fn fail_next(&self, count: u32) {
        lock(&self.state).remaining_failures = count;
    }

    pub fn set_rooms(&self, rooms: Vec<RoomSummary>) {
        lock(&self.state).rooms = rooms;
    }

    pub fn requests(&self) -> Vec<TokenRequest> {
        lock(&self.state).requests.clone()
    }

    pub fn set_chat_history_page(&self, before: Option<String>, page: ChatHistoryResponse) {
        lock(&self.state).chat_history_pages.insert(before, page);
    }

    pub fn push_save_result(&self, result: Result<ChatMessage, SessionError>) {
        lock(&self.state).save_results.push_back(result);
    }

    pub fn push_history_result(&self, result: Result<ChatHistoryResponse, SessionError>) {
        lock(&self.state).history_results.push_back(result);
    }

    pub fn push_delete_result(&self, result: Result<(), SessionError>) {
        lock(&self.state).delete_results.push_back(result);
    }

    pub fn save_calls(&self) -> Vec<(String, String, SaveChatRequest)> {
        lock(&self.state).save_calls.clone()
    }

    pub fn history_calls(&self) -> Vec<(String, String, Option<String>)> {
        lock(&self.state).history_calls.clone()
    }

    pub fn delete_calls(&self) -> Vec<(String, String, String)> {
        lock(&self.state).delete_calls.clone()
    }

    pub fn rooms_calls(&self) -> usize {
        lock(&self.state).rooms_calls
    }

    pub fn admin_calls(&self) -> Vec<AdminTokenCall> {
        lock(&self.state).admin_calls.clone()
    }

    pub fn push_admin_login_result(&self, result: Result<AdminLoginResponse, SessionError>) {
        lock(&self.state).admin_login_results.push_back(result);
    }

    pub fn push_admin_room_result(&self, result: Result<AdminRoom, SessionError>) {
        lock(&self.state).admin_room_results.push_back(result);
    }

    pub fn push_admin_unit_result(&self, result: Result<(), SessionError>) {
        lock(&self.state).admin_unit_results.push_back(result);
    }
}

#[async_trait]
impl TokenClient for FakeTokenClient {
    async fn fetch_token(&self, request: TokenRequest) -> Result<TokenResponse, SessionError> {
        let mut state = lock(&self.state);
        state.requests.push(request.clone());
        if state.reject_invite {
            return Err(SessionError::InviteRejected);
        }
        if state.remaining_failures > 0 {
            state.remaining_failures -= 1;
            return Err(SessionError::TokenServer("fake failure".to_owned()));
        }
        Ok(TokenResponse {
            token: format!("token-{}-{}", request.room, state.requests.len()),
            livekit_url: "ws://fake.invalid".to_owned(),
        })
    }

    async fn fetch_rooms(&self, _invite_code: &str) -> Result<Vec<RoomSummary>, SessionError> {
        let mut state = lock(&self.state);
        state.rooms_calls += 1;
        if state.reject_invite {
            return Err(SessionError::InviteRejected);
        }
        Ok(state.rooms.clone())
    }

    async fn save_chat(
        &self,
        jwt: &str,
        room: &str,
        request: SaveChatRequest,
    ) -> Result<ChatMessage, SessionError> {
        let mut state = lock(&self.state);
        state
            .save_calls
            .push((jwt.to_owned(), room.to_owned(), request.clone()));
        if let Some(result) = state.save_results.pop_front() {
            return result;
        }
        let sender = state
            .requests
            .iter()
            .rev()
            .find(|req| req.room == room)
            .map_or_else(|| "local".to_owned(), |req| req.display_name.clone());
        Ok(ChatMessage {
            id: request.id,
            sender,
            sent_at_unix_ms: request.sent_at_unix_ms,
            text: request.text,
            saved: true,
        })
    }

    async fn fetch_chat_history(
        &self,
        jwt: &str,
        room: &str,
        before: Option<&str>,
    ) -> Result<ChatHistoryResponse, SessionError> {
        let mut state = lock(&self.state);
        let cursor = before.map(str::to_owned);
        state
            .history_calls
            .push((jwt.to_owned(), room.to_owned(), cursor.clone()));
        if let Some(result) = state.history_results.pop_front() {
            return result;
        }
        Ok(state
            .chat_history_pages
            .get(&cursor)
            .cloned()
            .unwrap_or(ChatHistoryResponse {
                messages: Vec::new(),
                has_more: false,
            }))
    }

    async fn delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state
            .delete_calls
            .push((jwt.to_owned(), room.to_owned(), id.to_owned()));
        if let Some(result) = state.delete_results.pop_front() {
            return result;
        }
        Ok(())
    }

    async fn admin_login(
        &self,
        request: AdminLoginRequest,
    ) -> Result<AdminLoginResponse, SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::Login(request));
        if let Some(result) = state.admin_login_results.pop_front() {
            return result;
        }
        Ok(AdminLoginResponse {
            token: "admin-jwt-1".to_owned(),
            expires_at: 9_999_999_999,
        })
    }

    async fn admin_create_room(
        &self,
        jwt: &str,
        request: CreateRoomRequest,
    ) -> Result<AdminRoom, SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::CreateRoom {
            jwt: jwt.to_owned(),
            request: request.clone(),
        });
        if let Some(result) = state.admin_room_results.pop_front() {
            return result;
        }
        Ok(AdminRoom {
            id: "new-room".to_owned(),
            title: request.title,
            position: 0,
        })
    }

    async fn admin_update_room(
        &self,
        jwt: &str,
        id: &str,
        request: UpdateRoomRequest,
    ) -> Result<AdminRoom, SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::UpdateRoom {
            jwt: jwt.to_owned(),
            id: id.to_owned(),
            request: request.clone(),
        });
        if let Some(result) = state.admin_room_results.pop_front() {
            return result;
        }
        Ok(AdminRoom {
            id: id.to_owned(),
            title: request.title.unwrap_or_else(|| "Updated".to_owned()),
            position: request.position.unwrap_or(0),
        })
    }

    async fn admin_delete_room(&self, jwt: &str, id: &str) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::DeleteRoom {
            jwt: jwt.to_owned(),
            id: id.to_owned(),
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_mute_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::MuteParticipant {
            jwt: jwt.to_owned(),
            room: room.to_owned(),
            identity: identity.to_owned(),
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_stop_share_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state
            .admin_calls
            .push(AdminTokenCall::StopShareParticipant {
                jwt: jwt.to_owned(),
                room: room.to_owned(),
                identity: identity.to_owned(),
            });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_kick_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::KickParticipant {
            jwt: jwt.to_owned(),
            room: room.to_owned(),
            identity: identity.to_owned(),
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_move_participant(
        &self,
        jwt: &str,
        room: &str,
        identity: &str,
        request: MoveParticipantRequest,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::MoveParticipant {
            jwt: jwt.to_owned(),
            room: room.to_owned(),
            identity: identity.to_owned(),
            request,
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_delete_chat(&self, jwt: &str, room: &str, id: &str) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::DeleteChat {
            jwt: jwt.to_owned(),
            room: room.to_owned(),
            id: id.to_owned(),
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }

    async fn admin_rotate_invite(
        &self,
        jwt: &str,
        request: UpdateInviteRequest,
    ) -> Result<(), SessionError> {
        let mut state = lock(&self.state);
        state.admin_calls.push(AdminTokenCall::RotateInvite {
            jwt: jwt.to_owned(),
            request,
        });
        state.admin_unit_results.pop_front().unwrap_or(Ok(()))
    }
}
