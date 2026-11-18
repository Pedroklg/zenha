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
        webhook_posts: Vec::new(),
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
            ChatEntry::File(_) | ChatEntry::Webhook(_) => None,
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
        webhook_posts: Vec::new(),
    });

    let chat = &room(&state).chat;
    assert!(!chat.has_more);
    assert!(!chat.history_loading);
    let ids: Vec<&str> = chat
        .messages
        .iter()
        .filter_map(|e| match e {
            ChatEntry::Text(m) => Some(m.id.as_str()),
            ChatEntry::File(_) | ChatEntry::Webhook(_) => None,
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
        webhook_posts: Vec::new(),
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
            ChatEntry::File(_) | ChatEntry::Webhook(_) => None,
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

fn webhook_post(id: &str) -> zenha_protocol::WebhookPost {
    zenha_protocol::WebhookPost {
        id: id.to_owned(),
        name: "Gamers Club".to_owned(),
        sent_at_unix_ms: 7,
        content: Some("oi".to_owned()),
        embeds: Vec::new(),
    }
}

#[test]
fn webhook_post_joins_the_timeline_and_counts_as_unread_while_collapsed() {
    let mut state = state_with_code();
    in_room(&mut state);
    state.apply_event(UiEvent::ChatReceived(message(1)));
    state.apply_event(UiEvent::WebhookPosted(webhook_post("a1")));
    let chat = &room(&state).chat;
    assert_eq!(chat.messages.len(), 2);
    assert!(matches!(
        chat.messages.back(),
        Some(ChatEntry::Webhook(post)) if post.id == "a1"
    ));
    assert_eq!(chat.unread, 0);

    state.handle_action(UserAction::ToggleChat);
    state.apply_event(UiEvent::WebhookPosted(webhook_post("a2")));
    state.apply_event(UiEvent::WebhookPosted(webhook_post("a2")));
    assert_eq!(room(&state).chat.unread, 1);
    assert_eq!(room(&state).chat.messages.len(), 3);
}

#[test]
fn history_webhook_posts_interleave_by_time_without_duplicates() {
    let mut state = state_with_code();
    in_room(&mut state);
    state.apply_event(UiEvent::WebhookPosted(webhook_post("live")));
    let mut early = saved_message("h1", "ana", "Ana", "antes");
    early.sent_at_unix_ms = 5;
    let mut late = saved_message("h2", "ana", "Ana", "depois");
    late.sent_at_unix_ms = 9;
    state.apply_event(UiEvent::ChatHistory {
        messages: vec![early, late],
        has_more: false,
        before: None,
        webhook_posts: vec![webhook_post("old"), webhook_post("live")],
    });
    let order: Vec<String> = room(&state)
        .chat
        .messages
        .iter()
        .map(|entry| match entry {
            ChatEntry::Text(m) => m.id.clone(),
            ChatEntry::Webhook(p) => p.id.clone(),
            ChatEntry::File(f) => f.id.clone(),
        })
        .collect();
    assert_eq!(order, vec!["h1", "old", "h2", "live"]);
}

#[test]
fn webhook_post_is_ignored_outside_a_room_and_dropped_when_leaving() {
    let mut state = state_with_code();
    state.apply_event(UiEvent::WebhookPosted(webhook_post("a0")));
    assert!(!matches!(state.screen, Screen::InRoom(_)));

    in_room(&mut state);
    state.apply_event(UiEvent::WebhookPosted(webhook_post("a1")));
    assert_eq!(room(&state).chat.messages.len(), 1);
    state.apply_event(UiEvent::Left);
    in_room(&mut state);
    assert!(room(&state).chat.messages.is_empty());
}

#[test]
fn webhook_post_is_not_touched_by_history_or_delete() {
    let mut state = state_with_code();
    in_room(&mut state);
    state.apply_event(UiEvent::WebhookPosted(webhook_post("a1")));
    state.apply_event(UiEvent::ChatHistory {
        messages: vec![saved_message("h1", "ana", "ANA", "antiga")],
        has_more: false,
        before: None,
        webhook_posts: Vec::new(),
    });
    state.apply_event(UiEvent::ChatDeleted {
        id: "a1".to_owned(),
        sender: String::new(),
    });
    let chat = &room(&state).chat;
    assert_eq!(chat.messages.len(), 2);
    assert!(matches!(chat.messages.back(), Some(ChatEntry::Webhook(_))));
}
