//! Estado do chat, histórico, transferências de arquivo e visualizador de imagens.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use rooms_core::AdminAction;
use rooms_protocol::{MAX_CHAT_MESSAGE_LEN, validate_chat_text};

use super::room::RoomState;
use super::{AppState, Screen};
use crate::ui_model::{ChatMessageView, FileTransferView, TransferStateView, UiCommand};

pub const MAX_CHAT_HISTORY: usize = 500;
pub const CHAT_COUNTER_THRESHOLD: usize = MAX_CHAT_MESSAGE_LEN * 8 / 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    Text(ChatMessageView),
    File(FileTransferView),
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
    pub(super) fn new() -> Self {
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

    pub(super) fn apply_history(&mut self, messages: Vec<ChatMessageView>, has_more: bool) {
        self.history_loading = false;
        self.has_more = has_more;
        let existing: HashSet<String> = self
            .messages
            .iter()
            .filter_map(|entry| match entry {
                ChatEntry::Text(message) => Some(message.id.clone()),
                ChatEntry::File(_) => None,
            })
            .collect();
        let mut seen = HashSet::new();
        let mut fresh = Vec::with_capacity(messages.len());
        for message in messages {
            if !existing.contains(&message.id) && seen.insert(message.id.clone()) {
                fresh.push(message);
            }
        }
        for message in fresh.into_iter().rev() {
            self.messages.push_front(ChatEntry::Text(message));
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
            ChatEntry::File(_) => true,
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
        if self.audio.open || self.picker.open || self.admin.dialog.is_some() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{
        in_room, message, room, room_mut, saved_message, state_with_code, transfer,
    };
    use crate::ui_model::UiEvent;

    #[test]
    fn unread_counts_only_while_chat_is_collapsed() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(message(1)));
        assert_eq!(room(&state).chat.unread, 0);

        state.handle_action(UserAction::ToggleChat);
        assert!(!room(&state).chat.open);
        state.apply_event(UiEvent::ChatReceived(message(2)));
        state.apply_event(UiEvent::ChatReceived(message(3)));
        assert_eq!(room(&state).chat.unread, 2);

        state.handle_action(UserAction::ToggleChat);
        assert_eq!(room(&state).chat.unread, 0);
    }

    #[test]
    fn file_transfer_is_added_then_updated_by_id() {
        let mut state = state_with_code();
        in_room(&mut state);
        let progress = TransferStateView::InProgress { bytes: 10 };
        state.apply_event(UiEvent::FileTransfer(transfer("t1", false, progress)));
        state.apply_event(UiEvent::ChatReceived(message(1)));
        let done = TransferStateView::Done {
            path: PathBuf::from("a.png"),
        };
        state.apply_event(UiEvent::FileTransfer(transfer("t1", false, done.clone())));
        let chat = &room(&state).chat;
        assert_eq!(chat.messages.len(), 2);
        assert!(matches!(
            chat.messages.front(),
            Some(ChatEntry::File(f)) if f.state == done
        ));
    }

    #[test]
    fn file_transfer_unread_counts_only_incoming_while_collapsed() {
        let mut state = state_with_code();
        in_room(&mut state);
        let progress = TransferStateView::InProgress { bytes: 0 };
        state.apply_event(UiEvent::FileTransfer(transfer(
            "t0",
            false,
            progress.clone(),
        )));
        assert_eq!(room(&state).chat.unread, 0);
        state.handle_action(UserAction::ToggleChat);
        state.apply_event(UiEvent::FileTransfer(transfer(
            "t1",
            false,
            progress.clone(),
        )));
        state.apply_event(UiEvent::FileTransfer(transfer(
            "t1",
            false,
            progress.clone(),
        )));
        state.apply_event(UiEvent::FileTransfer(transfer("t2", true, progress)));
        assert_eq!(room(&state).chat.unread, 1);
        assert_eq!(room(&state).chat.messages.len(), 3);
    }

    #[test]
    fn can_paste_chat_image_requires_open_chat_without_modals() {
        let mut state = state_with_code();
        assert!(!state.can_paste_chat_image());

        state.handle_action(UserAction::SubmitOnboarding);
        assert!(!state.can_paste_chat_image());

        in_room(&mut state);
        assert!(matches!(state.screen, Screen::InRoom(_)));
        if let Screen::InRoom(room) = &mut state.screen {
            assert!(!room.chat.save);
            room.chat.open = false;
        }
        assert!(!state.can_paste_chat_image());

        if let Screen::InRoom(room) = &mut state.screen {
            room.chat.open = true;
        }
        assert!(state.can_paste_chat_image());

        state.audio.open = true;
        assert!(!state.can_paste_chat_image());

        state.audio.open = false;
        state.picker.open = true;
        assert!(!state.can_paste_chat_image());

        state.picker.open = false;
        if let Screen::InRoom(room) = &mut state.screen {
            room.chat.viewer = Some(ImageViewer {
                id: "img-1".to_owned(),
                name: "print.png".to_owned(),
                size: 128,
                path: PathBuf::from("print.png"),
                outgoing: true,
                error: None,
            });
        }
        assert!(!state.can_paste_chat_image());
    }

    #[test]
    fn attach_file_needs_a_room() {
        let mut state = state_with_code();
        let action = UserAction::AttachFile(PathBuf::from("a.png"));
        assert!(state.handle_action(action.clone()).is_empty());
        in_room(&mut state);
        assert_eq!(
            state.handle_action(action),
            vec![UiCommand::SendFile {
                path: PathBuf::from("a.png")
            }]
        );
    }

    #[test]
    fn effect_actions_do_not_change_state() {
        let mut state = state_with_code();
        in_room(&mut state);
        let before = room(&state).clone();
        assert!(state.handle_action(UserAction::PickFile).is_empty());
        let save = UserAction::SaveReceivedFile {
            source: PathBuf::from("a.png"),
            name: "a.png".to_owned(),
        };
        assert!(state.handle_action(save).is_empty());
        let open_external = UserAction::OpenImageExternally(PathBuf::from("a.png"));
        assert!(state.handle_action(open_external).is_empty());
        assert_eq!(*room(&state), before);
    }

    #[test]
    fn image_viewer_opens_received_and_sent_images_and_closes() {
        let mut state = state_with_code();
        in_room(&mut state);
        let mut non_image = transfer(
            "doc-1",
            false,
            TransferStateView::Done {
                path: PathBuf::from("notes.txt"),
            },
        );
        non_image.is_image = false;
        room_mut(&mut state).chat.apply_transfer(non_image);
        room_mut(&mut state).chat.apply_transfer(transfer(
            "img-rx",
            false,
            TransferStateView::Done {
                path: PathBuf::from("rx.png"),
            },
        ));
        room_mut(&mut state).chat.apply_transfer(transfer(
            "img-tx",
            true,
            TransferStateView::Done {
                path: PathBuf::from("tx.png"),
            },
        ));

        state.handle_action(UserAction::OpenImageViewer("missing".to_owned()));
        assert_eq!(state.open_image_id(), None);

        state.handle_action(UserAction::OpenImageViewer("doc-1".to_owned()));
        assert_eq!(state.open_image_id(), None);

        state.handle_action(UserAction::OpenImageViewer("img-rx".to_owned()));
        assert_eq!(
            room(&state).chat.viewer,
            Some(ImageViewer {
                id: "img-rx".to_owned(),
                name: "foto.png".to_owned(),
                size: 100,
                path: PathBuf::from("rx.png"),
                outgoing: false,
                error: None,
            })
        );
        assert_eq!(state.open_image_id(), Some("img-rx"));

        state.handle_action(UserAction::OpenImageViewer("img-tx".to_owned()));
        assert_eq!(state.open_image_id(), Some("img-tx"));
        assert!(
            room(&state)
                .chat
                .viewer
                .as_ref()
                .is_some_and(|v| v.outgoing)
        );

        state.handle_action(UserAction::CloseImageViewer);
        assert_eq!(state.open_image_id(), None);
        assert_eq!(room(&state).chat.viewer, None);
    }

    #[test]
    fn image_viewer_closes_on_history_eviction_path_change_and_clear() {
        let mut state = state_with_code();
        in_room(&mut state);
        room_mut(&mut state).chat.apply_transfer(transfer(
            "img-old",
            false,
            TransferStateView::Done {
                path: PathBuf::from("old.png"),
            },
        ));
        state.handle_action(UserAction::OpenImageViewer("img-old".to_owned()));
        assert_eq!(state.open_image_id(), Some("img-old"));

        room_mut(&mut state).chat.apply_transfer(transfer(
            "img-old",
            false,
            TransferStateView::Done {
                path: PathBuf::from("other.png"),
            },
        ));
        assert_eq!(state.open_image_id(), None);

        room_mut(&mut state).chat.apply_transfer(transfer(
            "img-evict",
            true,
            TransferStateView::Done {
                path: PathBuf::from("evict.png"),
            },
        ));
        state.handle_action(UserAction::OpenImageViewer("img-evict".to_owned()));
        assert_eq!(state.open_image_id(), Some("img-evict"));

        for i in 0..MAX_CHAT_HISTORY {
            room_mut(&mut state).chat.apply_transfer(transfer(
                &format!("fill-{i}"),
                true,
                TransferStateView::Done {
                    path: PathBuf::from(format!("fill-{i}.png")),
                },
            ));
        }
        assert_eq!(state.open_image_id(), None);

        room_mut(&mut state).chat.apply_transfer(transfer(
            "img-clear",
            false,
            TransferStateView::Done {
                path: PathBuf::from("clear.png"),
            },
        ));
        state.handle_action(UserAction::OpenImageViewer("img-clear".to_owned()));
        assert_eq!(state.open_image_id(), Some("img-clear"));
        room_mut(&mut state).chat.clear();
        assert_eq!(state.open_image_id(), None);
    }

    #[test]
    fn image_viewer_error_updates_only_while_open() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.set_image_viewer_error("ignored");
        room_mut(&mut state).chat.apply_transfer(transfer(
            "img-1",
            false,
            TransferStateView::Done {
                path: PathBuf::from("1.png"),
            },
        ));
        state.handle_action(UserAction::OpenImageViewer("img-1".to_owned()));
        state.set_image_viewer_error("Não foi possível abrir no Windows.");
        assert_eq!(
            room(&state)
                .chat
                .viewer
                .as_ref()
                .and_then(|viewer| viewer.error.as_deref()),
            Some("Não foi possível abrir no Windows.")
        );
        state.clear_image_viewer_error();
        assert_eq!(
            room(&state)
                .chat
                .viewer
                .as_ref()
                .and_then(|viewer| viewer.error.as_deref()),
            None
        );
    }

    #[test]
    fn chat_notice_is_set_dismissed_and_cleared_on_room_switch() {
        let mut state = state_with_code();
        state.set_chat_notice("ignorado".to_owned());
        in_room(&mut state);
        state.set_chat_notice("Arquivo salvo".to_owned());
        assert_eq!(room(&state).chat.notice.as_deref(), Some("Arquivo salvo"));
        state.handle_action(UserAction::DismissChatNotice);
        assert_eq!(room(&state).chat.notice, None);
        state.set_chat_notice("outro".to_owned());
        state.apply_event(UiEvent::FileTransfer(transfer(
            "t1",
            false,
            TransferStateView::Failed("x".to_owned()),
        )));
        state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        let chat = &room(&state).chat;
        assert!(chat.messages.is_empty());
        assert_eq!(chat.notice, None);
    }

    #[test]
    fn chat_history_is_capped() {
        let mut state = state_with_code();
        in_room(&mut state);
        for id in 0..MAX_CHAT_HISTORY + 25 {
            state.apply_event(UiEvent::ChatReceived(message(id)));
        }
        let chat = &room(&state).chat;
        assert_eq!(chat.messages.len(), MAX_CHAT_HISTORY);
        assert!(matches!(
            chat.messages.front(),
            Some(ChatEntry::Text(m)) if m.id == "25"
        ));
    }

    #[test]
    fn send_chat_trims_clears_draft_and_rejects_empty() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert!(state.handle_action(UserAction::SendChat).is_empty());

        if let Screen::InRoom(room) = &mut state.screen {
            room.chat.draft = "  oi  ".to_owned();
        }
        assert_eq!(
            state.handle_action(UserAction::SendChat),
            vec![UiCommand::SendChat {
                text: "oi".to_owned(),
                save: false,
            }]
        );
        assert!(room(&state).chat.draft.is_empty());

        if let Screen::InRoom(room) = &mut state.screen {
            room.chat.draft = "x".repeat(MAX_CHAT_MESSAGE_LEN + 1);
        }
        assert!(state.handle_action(UserAction::SendChat).is_empty());
    }

    #[test]
    fn chat_counter_appears_near_the_limit() {
        let mut chat = ChatState::new();
        chat.draft = "x".repeat(CHAT_COUNTER_THRESHOLD - 1);
        assert!(!chat.show_counter());
        chat.draft.push('x');
        assert!(chat.show_counter());
    }

    #[test]
    fn chat_history_initial_and_older_pages_order_and_pending_state() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.handle_action(UserAction::ToggleChat);
        state.apply_event(UiEvent::ChatReceived(message(99)));
        assert_eq!(room(&state).chat.unread, 1);

        state.apply_event(UiEvent::ChatHistory {
            messages: vec![
                saved_message("h2", "ana", "Ana", "segunda"),
                saved_message("h3", "pedro", "Pedro", "terceira"),
            ],
            has_more: true,
            before: None,
        });

        let chat = &room(&state).chat;
        assert!(chat.has_more);
        assert!(!chat.history_loading);
        assert_eq!(chat.unread, 1);
        let ids: Vec<&str> = chat
            .messages
            .iter()
            .filter_map(|e| match e {
                ChatEntry::Text(m) => Some(m.id.as_str()),
                ChatEntry::File(_) => None,
            })
            .collect();
        assert_eq!(ids, vec!["h2", "h3", "99"]);

        assert_eq!(
            state.handle_action(UserAction::LoadOlderChat),
            vec![UiCommand::LoadChatHistory {
                before: Some("h2".to_owned()),
            }]
        );
        assert!(room(&state).chat.history_loading);
        assert!(state.handle_action(UserAction::LoadOlderChat).is_empty());

        state.apply_event(UiEvent::ChatHistory {
            messages: vec![
                saved_message("h0", "ana", "Ana", "zero"),
                saved_message("h1", "pedro", "Pedro", "um"),
            ],
            has_more: false,
            before: Some("h2".to_owned()),
        });

        let chat = &room(&state).chat;
        assert!(!chat.has_more);
        assert!(!chat.history_loading);
        let ids: Vec<&str> = chat
            .messages
            .iter()
            .filter_map(|e| match e {
                ChatEntry::Text(m) => Some(m.id.as_str()),
                ChatEntry::File(_) => None,
            })
            .collect();
        assert_eq!(ids, vec!["h0", "h1", "h2", "h3", "99"]);

        state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        assert!(!room(&state).chat.has_more);
        assert!(!room(&state).chat.history_loading);
        assert!(room(&state).chat.messages.is_empty());
    }

    #[test]
    fn chat_deduplicates_overlapping_live_and_history_ids() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(saved_message(
            "s2",
            "ana",
            "Ana",
            "ao vivo e salva",
        )));
        state.apply_event(UiEvent::ChatHistory {
            messages: vec![
                saved_message("s1", "ana", "Ana", "antiga"),
                saved_message("s2", "ana", "Ana", "ao vivo e salva"),
                saved_message("s2", "ana", "Ana", "ao vivo e salva"),
            ],
            has_more: false,
            before: None,
        });
        state.apply_event(UiEvent::ChatReceived(saved_message(
            "s1", "ana", "Ana", "antiga",
        )));

        let ids: Vec<&str> = room(&state)
            .chat
            .messages
            .iter()
            .filter_map(|e| match e {
                ChatEntry::Text(m) => Some(m.id.as_str()),
                ChatEntry::File(_) => None,
            })
            .collect();
        assert_eq!(ids, vec!["s1", "s2"]);
    }

    #[test]
    fn chat_delete_requires_saved_own_message_and_matching_sender() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(ChatMessageView {
            id: "own-unsaved".to_owned(),
            sender_id: "pedro".to_owned(),
            sender: "Pedro".to_owned(),
            sent_at_unix_ms: 1,
            text: "nao salva".to_owned(),
            saved: false,
        }));
        state.apply_event(UiEvent::ChatReceived(saved_message(
            "own-saved",
            "pedro",
            "Pedro",
            "minha salva",
        )));
        state.apply_event(UiEvent::ChatReceived(saved_message(
            "other-saved",
            "ana",
            "Ana",
            "da ana",
        )));

        assert!(
            state
                .handle_action(UserAction::DeleteChat("own-unsaved".to_owned()))
                .is_empty()
        );
        assert!(
            state
                .handle_action(UserAction::DeleteChat("other-saved".to_owned()))
                .is_empty()
        );
        assert_eq!(
            state.handle_action(UserAction::DeleteChat("own-saved".to_owned())),
            vec![UiCommand::DeleteChat {
                id: "own-saved".to_owned(),
            }]
        );

        state.apply_event(UiEvent::ChatDeleted {
            id: "own-saved".to_owned(),
            sender: "ana".to_owned(),
        });
        assert!(room(&state).chat.contains_text_id("own-saved"));

        state.apply_event(UiEvent::ChatDeleted {
            id: "own-saved".to_owned(),
            sender: "pedro".to_owned(),
        });
        assert!(!room(&state).chat.contains_text_id("own-saved"));
        assert!(room(&state).chat.contains_text_id("other-saved"));
    }

    #[test]
    fn save_checkbox_resets_after_send_and_draft_recovers_on_chat_failure() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert!(!room(&state).chat.save);

        if let Screen::InRoom(room) = &mut state.screen {
            room.chat.draft = "aviso".to_owned();
            room.chat.save = true;
        }
        assert_eq!(
            state.handle_action(UserAction::SendChat),
            vec![UiCommand::SendChat {
                text: "aviso".to_owned(),
                save: true,
            }]
        );
        assert!(!room(&state).chat.save);
        assert!(room(&state).chat.draft.is_empty());

        state.apply_event(UiEvent::ChatFailed {
            message: "Não foi possível salvar a mensagem.".to_owned(),
        });
        assert_eq!(
            room(&state).chat.notice.as_deref(),
            Some("Não foi possível salvar a mensagem.")
        );
        assert_eq!(room(&state).chat.draft, "aviso");
        assert!(room(&state).chat.save);

        assert_eq!(
            state.handle_action(UserAction::SendChat),
            vec![UiCommand::SendChat {
                text: "aviso".to_owned(),
                save: true,
            }]
        );
        state.apply_event(UiEvent::ChatReceived(saved_message(
            "s-ok", "pedro", "Pedro", "aviso",
        )));
        if let Screen::InRoom(room) = &mut state.screen {
            room.chat.has_more = true;
        }
        state.handle_action(UserAction::LoadOlderChat);
        assert!(room(&state).chat.history_loading);
        state.apply_event(UiEvent::ChatFailed {
            message: "Muitas mensagens seguidas. Espere alguns segundos.".to_owned(),
        });
        assert!(!room(&state).chat.history_loading);
        assert!(room(&state).chat.draft.is_empty());
    }

    fn pasted_image(name: &str) -> PendingPastedImage {
        PendingPastedImage {
            path: PathBuf::from(name),
            name: name.to_owned(),
            size: 2048,
        }
    }

    #[test]
    fn pasting_image_creates_pending_without_command_and_send_emits_send_file() {
        let mut state = state_with_code();
        let outside = pasted_image("fora.png");
        assert!(
            state
                .handle_action(UserAction::StagePastedImage(outside.clone()))
                .is_empty()
        );
        assert_eq!(state.take_pending_paste_deletes(), vec![outside.path]);

        in_room(&mut state);
        let first = pasted_image("print-1.png");
        assert!(
            state
                .handle_action(UserAction::StagePastedImage(first.clone()))
                .is_empty()
        );
        assert_eq!(room(&state).chat.pending_paste.as_ref(), Some(&first));
        assert!(state.take_pending_paste_deletes().is_empty());

        let expected = state.handle_action(UserAction::AttachFile(first.path.clone()));
        assert_eq!(state.handle_action(UserAction::SendPastedImage), expected);
        assert_eq!(room(&state).chat.pending_paste, None);
        assert!(state.take_pending_paste_deletes().is_empty());
        assert!(state.handle_action(UserAction::SendPastedImage).is_empty());
    }

    #[test]
    fn replacing_canceling_switching_or_leaving_clears_pending_paste_and_queues_delete() {
        let mut state = state_with_code();
        in_room(&mut state);

        let first = pasted_image("print-1.png");
        let second = pasted_image("print-2.png");
        state.handle_action(UserAction::StagePastedImage(first.clone()));
        assert!(
            state
                .handle_action(UserAction::StagePastedImage(second.clone()))
                .is_empty()
        );
        assert_eq!(room(&state).chat.pending_paste.as_ref(), Some(&second));
        assert_eq!(state.take_pending_paste_deletes(), vec![first.path]);

        assert!(
            state
                .handle_action(UserAction::CancelPastedImage)
                .is_empty()
        );
        assert_eq!(room(&state).chat.pending_paste, None);
        assert_eq!(state.take_pending_paste_deletes(), vec![second.path]);

        let third = pasted_image("print-3.png");
        state.handle_action(UserAction::StagePastedImage(third.clone()));
        state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        assert_eq!(room(&state).chat.pending_paste, None);
        assert_eq!(state.take_pending_paste_deletes(), vec![third.path]);

        let fourth = pasted_image("print-4.png");
        state.handle_action(UserAction::StagePastedImage(fourth.clone()));
        state.handle_action(UserAction::Leave);
        assert_eq!(state.take_pending_paste_deletes(), vec![fourth.path]);

        in_room(&mut state);
        let fifth = pasted_image("print-5.png");
        state.handle_action(UserAction::StagePastedImage(fifth.clone()));
        state.apply_event(UiEvent::Left);
        assert_eq!(state.take_pending_paste_deletes(), vec![fifth.path]);
    }
}
