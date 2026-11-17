//! `FakeTokenClient`: fake token client with programmable responses and recorded calls.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use zenha_protocol::{
    AdminLoginRequest, AdminLoginResponse, AdminRoom, AvatarUploadResponse, ChatHistoryResponse,
    ChatMessage, CreateRoomRequest, MoveParticipantRequest, RoomSummary, SaveChatRequest,
    TokenRequest, UpdateInviteRequest, UpdateRoomRequest,
};

use crate::api::SessionError;

use super::lock;

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
pub(super) struct TokenState {
    pub(super) requests: Vec<TokenRequest>,
    pub(super) reject_invite: bool,
    pub(super) remaining_failures: u32,
    pub(super) rooms: Vec<RoomSummary>,
    pub(super) rooms_calls: usize,
    pub(super) rooms_keys: Vec<BTreeMap<String, String>>,
    pub(super) room_locks: HashMap<String, FakeRoomLock>,
    pub(super) chat_history_pages: HashMap<Option<String>, ChatHistoryResponse>,
    pub(super) save_calls: Vec<(String, String, SaveChatRequest)>,
    pub(super) history_calls: Vec<(String, String, Option<String>)>,
    pub(super) delete_calls: Vec<(String, String, String)>,
    pub(super) save_results: VecDeque<Result<ChatMessage, SessionError>>,
    pub(super) history_results: VecDeque<Result<ChatHistoryResponse, SessionError>>,
    pub(super) delete_results: VecDeque<Result<(), SessionError>>,
    pub(super) admin_calls: Vec<AdminTokenCall>,
    pub(super) admin_login_results: VecDeque<Result<AdminLoginResponse, SessionError>>,
    pub(super) admin_room_results: VecDeque<Result<AdminRoom, SessionError>>,
    pub(super) admin_unit_results: VecDeque<Result<(), SessionError>>,
    pub(super) avatars: HashMap<(String, String), Vec<u8>>,
    pub(super) upload_avatar_calls: Vec<(String, Vec<u8>)>,
    pub(super) delete_avatar_calls: Vec<String>,
    pub(super) download_avatar_calls: Vec<(String, String, Option<String>)>,
    pub(super) upload_avatar_results: VecDeque<Result<AvatarUploadResponse, SessionError>>,
    pub(super) delete_avatar_results: VecDeque<Result<(), SessionError>>,
}

#[derive(Debug, Clone)]
pub(super) struct FakeRoomLock {
    pub(super) owner: String,
    pub(super) password: String,
    pub(super) key: String,
}

#[derive(Debug, Clone)]
pub struct FakeTokenClient {
    pub(super) state: Arc<Mutex<TokenState>>,
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
                rooms_keys: Vec::new(),
                room_locks: HashMap::new(),
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
                avatars: HashMap::new(),
                upload_avatar_calls: Vec::new(),
                delete_avatar_calls: Vec::new(),
                download_avatar_calls: Vec::new(),
                upload_avatar_results: VecDeque::new(),
                delete_avatar_results: VecDeque::new(),
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

    /// Locks `room`: the token requires `key`, obtained by unlocking with `password` (owner: `owner`).
    pub fn lock_room(&self, room: &str, owner: &str, password: &str, key: &str) {
        lock(&self.state).room_locks.insert(
            room.to_owned(),
            FakeRoomLock {
                owner: owner.to_owned(),
                password: password.to_owned(),
                key: key.to_owned(),
            },
        );
    }

    /// Keys sent in each `fetch_rooms`.
    pub fn rooms_keys(&self) -> Vec<BTreeMap<String, String>> {
        lock(&self.state).rooms_keys.clone()
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

    pub fn set_avatar_bytes(&self, identity: &str, version: &str, bytes: Vec<u8>) {
        lock(&self.state)
            .avatars
            .insert((identity.to_owned(), version.to_owned()), bytes);
    }

    pub fn push_upload_avatar_result(&self, result: Result<AvatarUploadResponse, SessionError>) {
        lock(&self.state).upload_avatar_results.push_back(result);
    }

    pub fn push_delete_avatar_result(&self, result: Result<(), SessionError>) {
        lock(&self.state).delete_avatar_results.push_back(result);
    }

    pub fn upload_avatar_calls(&self) -> Vec<(String, Vec<u8>)> {
        lock(&self.state).upload_avatar_calls.clone()
    }

    pub fn delete_avatar_calls(&self) -> Vec<String> {
        lock(&self.state).delete_avatar_calls.clone()
    }

    pub fn download_avatar_calls(&self) -> Vec<(String, String, Option<String>)> {
        lock(&self.state).download_avatar_calls.clone()
    }
}
