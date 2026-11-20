//! Chat state, history, file transfers and image viewer.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use zenha_core::AdminAction;
use zenha_protocol::{MAX_CHAT_MESSAGE_LEN, WebhookPost, validate_chat_text};

use super::room::RoomState;
use super::{AppState, Screen};
use crate::ui_model::{ChatMessageView, FileTransferView, TransferStateView, UiCommand};

pub const MAX_CHAT_HISTORY: usize = 500;
pub const CHAT_COUNTER_THRESHOLD: usize = MAX_CHAT_MESSAGE_LEN * 8 / 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    Text(ChatMessageView),
    File(FileTransferView),
    Webhook(WebhookPost),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageViewer {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub path: PathBuf,
    pub outgoing: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPastedImage {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatState {
    pub messages: VecDeque<ChatEntry>,
    pub unread: usize,
    pub draft: String,
    pub save: bool,
    pub pending_paste: Option<PendingPastedImage>,
    pub has_more: bool,
    pub history_loading: bool,
    pub open: bool,
    pub notice: Option<String>,
    pub viewer: Option<ImageViewer>,
    pending_saved_drafts: VecDeque<String>,
}

impl ChatState {
    pub(crate) fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            unread: 0,
            draft: String::new(),
            save: false,
            pending_paste: None,
            has_more: false,
            history_loading: false,
            open: true,
            notice: None,
            viewer: None,
            pending_saved_drafts: VecDeque::new(),
        }
    }

    pub(super) fn contains_text_id(&self, id: &str) -> bool {
        self.messages
            .iter()
            .any(|entry| matches!(entry, ChatEntry::Text(message) if message.id == id))
    }

    fn push(&mut self, entry: ChatEntry, counts_as_unread: bool) {
        if self.messages.len() >= MAX_CHAT_HISTORY {
            self.messages.pop_front();
        }
        self.messages.push_back(entry);
        if counts_as_unread && !self.open {
            self.unread += 1;
        }
        self.prune_viewer();
    }

    pub(super) fn push_message(&mut self, message: ChatMessageView, local_identity: &str) {
        if message.saved && !local_identity.is_empty() && message.sender_id == local_identity {
            self.pending_saved_drafts.pop_front();
        }
        if self.contains_text_id(&message.id) {
            return;
        }
        self.push(ChatEntry::Text(message), true);
    }

    pub(super) fn push_webhook(&mut self, post: WebhookPost) -> bool {
        let known = self
            .messages
            .iter()
            .any(|entry| matches!(entry, ChatEntry::Webhook(existing) if existing.id == post.id));
        if known {
            return false;
        }
        self.push(ChatEntry::Webhook(post), true);
        true
    }

    pub(super) fn apply_history(
        &mut self,
        messages: Vec<ChatMessageView>,
        webhook_posts: Vec<WebhookPost>,
        has_more: bool,
    ) {
        self.history_loading = false;
        self.has_more = has_more;
        let mut seen: HashSet<String> = self.messages.iter().filter_map(entry_key).collect();
        let mut fresh: Vec<ChatEntry> = messages
            .into_iter()
            .map(ChatEntry::Text)
            .filter(|entry| entry_key(entry).is_some_and(|key| seen.insert(key)))
            .collect();
        if !webhook_posts.is_empty() {
            fresh.extend(
                webhook_posts
                    .into_iter()
                    .map(ChatEntry::Webhook)
                    .filter(|entry| entry_key(entry).is_some_and(|key| seen.insert(key))),
            );
            fresh.sort_by_key(entry_time);
        }
        for entry in fresh.into_iter().rev() {
            self.messages.push_front(entry);
        }
    }

    pub fn oldest_saved_id(&self) -> Option<String> {
        self.messages.iter().find_map(|entry| match entry {
            ChatEntry::Text(message) if message.saved => Some(message.id.clone()),
            _ => None,
        })
    }

    pub(super) fn delete_message(&mut self, id: &str, sender: &str) {
        self.messages.retain(|entry| match entry {
            ChatEntry::Text(message) => {
                !(message.id == id && (sender.is_empty() || message.sender_id == sender))
            }
            ChatEntry::File(_) | ChatEntry::Webhook(_) => true,
        });
    }

    pub(super) fn handle_failed(&mut self, message: String) {
        self.notice = Some(message);
        self.history_loading = false;
        if let Some(draft) = self.pending_saved_drafts.pop_front()
            && self.draft.is_empty()
        {
            self.draft = draft;
            self.save = true;
        }
    }

    pub(super) fn apply_transfer(&mut self, view: FileTransferView) {
        let existing = self.messages.iter_mut().find_map(|entry| match entry {
            ChatEntry::File(file) if file.id == view.id => Some(file),
            _ => None,
        });
        match existing {
            Some(file) => {
                file.state = view.state;
                self.prune_viewer();
            }
            None => {
                let counts = !view.outgoing;
                self.push(ChatEntry::File(view), counts);
            }
        }
    }

    pub fn set_notice(&mut self, text: String) {
        self.notice = Some(text);
    }

    pub(super) fn open_image(&mut self, id: &str) {
        let found = self.messages.iter().find_map(|entry| match entry {
            ChatEntry::File(file) if file.id == id && file.is_image => match &file.state {
                TransferStateView::Done { path } => Some(ImageViewer {
                    id: file.id.clone(),
                    name: file.name.clone(),
                    size: file.size,
                    path: path.clone(),
                    outgoing: file.outgoing,
                    error: None,
                }),
                _ => None,
            },
            _ => None,
        });
        if let Some(viewer) = found {
            self.viewer = Some(viewer);
        }
    }

    pub(super) fn close_image(&mut self) {
        self.viewer = None;
    }

    fn prune_viewer(&mut self) {
        let Some(open) = &self.viewer else {
            return;
        };
        let still_present = self.messages.iter().any(|entry| match entry {
            ChatEntry::File(file) if file.id == open.id && file.is_image => {
                matches!(&file.state, TransferStateView::Done { path } if *path == open.path)
            }
            _ => false,
        });
        if !still_present {
            self.viewer = None;
        }
    }

    pub(super) fn clear(&mut self) -> Option<PathBuf> {
        self.messages.clear();
        self.unread = 0;
        self.draft.clear();
        self.save = false;
        let discarded_paste = self.pending_paste.take().map(|pending| pending.path);
        self.has_more = false;
        self.history_loading = false;
        self.notice = None;
        self.viewer = None;
        self.pending_saved_drafts.clear();
        discarded_paste
    }

    pub fn draft_len(&self) -> usize {
        self.draft.chars().count()
    }

    pub fn show_counter(&self) -> bool {
        self.draft_len() >= CHAT_COUNTER_THRESHOLD
    }
}

impl RoomState {
    pub(super) fn send_chat(&mut self) -> Vec<UiCommand> {
        if validate_chat_text(&self.chat.draft).is_err() {
            return Vec::new();
        }
        let text = self.chat.draft.trim().to_owned();
        let save = self.chat.save;
        self.chat.draft.clear();
        self.chat.save = false;
        if save {
            self.chat.pending_saved_drafts.push_back(text.clone());
        }
        vec![UiCommand::SendChat { text, save }]
    }

    pub(super) fn load_older_chat(&mut self) -> Vec<UiCommand> {
        if !self.chat.has_more || self.chat.history_loading {
            return Vec::new();
        }
        let Some(before) = self.chat.oldest_saved_id() else {
            return Vec::new();
        };
        self.chat.history_loading = true;
        vec![UiCommand::LoadChatHistory {
            before: Some(before),
        }]
    }

    pub(super) fn delete_chat(&self, id: String, is_admin: bool) -> Vec<UiCommand> {
        if self.local_identity.is_empty() {
            return Vec::new();
        }
        let target = self.chat.messages.iter().find_map(|entry| match entry {
            ChatEntry::Text(message) if message.id == id && message.saved => Some(message),
            _ => None,
        });
        let Some(message) = target else {
            return Vec::new();
        };
        if message.sender_id == self.local_identity {
            vec![UiCommand::DeleteChat { id }]
        } else if is_admin {
            vec![UiCommand::Admin(AdminAction::DeleteChat {
                room: self.room.clone(),
                id,
            })]
        } else {
            Vec::new()
        }
    }
}

impl AppState {
    pub fn open_image_id(&self) -> Option<&str> {
        match &self.screen {
            Screen::InRoom(room) => room.chat.viewer.as_ref().map(|viewer| viewer.id.as_str()),
            _ => None,
        }
    }

    pub fn clear_image_viewer_error(&mut self) {
        if let Some(viewer) = self.open_viewer_mut() {
            viewer.error = None;
        }
    }

    pub fn set_image_viewer_error(&mut self, message: impl Into<String>) {
        if let Some(viewer) = self.open_viewer_mut() {
            viewer.error = Some(message.into());
        }
    }

    fn open_viewer_mut(&mut self) -> Option<&mut ImageViewer> {
        match &mut self.screen {
            Screen::InRoom(room) => room.chat.viewer.as_mut(),
            _ => None,
        }
    }

    pub fn set_chat_notice(&mut self, text: String) {
        if let Screen::InRoom(room) = &mut self.screen {
            room.chat.set_notice(text);
        }
    }

    pub(super) fn discard_pending_paste(&mut self) {
        if let Screen::InRoom(room) = &mut self.screen
            && let Some(pending) = room.chat.pending_paste.take()
        {
            self.pending_paste_deletes.push(pending.path);
        }
    }

    pub fn has_pending_paste(&self) -> bool {
        matches!(&self.screen, Screen::InRoom(room) if room.chat.pending_paste.is_some())
    }

    pub fn chat_draft_is_empty(&self) -> bool {
        matches!(&self.screen, Screen::InRoom(room) if room.chat.draft.is_empty())
    }

    pub fn can_paste_chat_image(&self) -> bool {
        if self.audio.open
            || self.picker.open
            || self.admin.dialog.is_some()
            || self.is_update_confirming()
        {
            return false;
        }
        match &self.screen {
            Screen::InRoom(room) => {
                room.chat.open
                    && room.chat.viewer.is_none()
                    && !room.share.watching.as_ref().is_some_and(|w| w.fullscreen)
            }
            Screen::Onboarding | Screen::Lobby | Screen::Connecting { .. } => false,
        }
    }
}

fn entry_key(entry: &ChatEntry) -> Option<String> {
    match entry {
        ChatEntry::Text(message) => Some(format!("text:{}", message.id)),
        ChatEntry::Webhook(post) => Some(format!("webhook:{}", post.id)),
        ChatEntry::File(_) => None,
    }
}

fn entry_time(entry: &ChatEntry) -> u64 {
    match entry {
        ChatEntry::Text(message) => message.sent_at_unix_ms,
        ChatEntry::Webhook(post) => post.sent_at_unix_ms,
        ChatEntry::File(_) => 0,
    }
}

#[cfg(test)]
mod tests;
