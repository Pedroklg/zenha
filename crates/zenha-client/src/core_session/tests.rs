//! Tests for event and command translation.

use zenha_core::{
    AudioDevice, CaptureEnd, CaptureKind, CaptureTarget, Command, Event, FrameSlot, Participant,
    SessionError, SharePreset, TransferState, VideoFrame,
};
use zenha_protocol::{ChatMessage, SavedChatMessage};

use crate::ui_model::{
    AudioDeviceView, CaptureTargetView, ChatMessageView, ParticipantView, SharePresetView,
    TargetKindView, TransferStateView, UiCommand, UiEvent,
};

use super::TestFeed;
use super::commands::to_core;
use super::errors::describe;
use super::translate::Translator;

fn participant(identity: &str, name: &str) -> Participant {
    Participant {
        identity: identity.to_owned(),
        name: name.to_owned(),
        muted: false,
        deafened: false,
        speaking: false,
        avatar_version: None,
    }
}

fn joined(translator: &mut Translator) {
    translator.translate(Event::Joined {
        room: "geral".to_owned(),
        local_identity: "pedro".to_owned(),
        participants: vec![participant("pedro", "Pedro"), participant("ana", "Ana")],
    });
}

#[test]
fn mute_and_speaking_changes_become_full_participant_updates() {
    let mut translator = Translator::default();
    joined(&mut translator);
    let events = translator.translate(Event::ParticipantMuteChanged {
        identity: "ana".to_owned(),
        muted: true,
    });
    assert_eq!(
        events,
        vec![UiEvent::ParticipantUpdated(ParticipantView {
            identity: "ana".to_owned(),
            name: "Ana".to_owned(),
            muted: true,
            deafened: false,
            speaking: false,
            avatar_version: None,
        })]
    );
    let events = translator.translate(Event::ParticipantSpeakingChanged {
        identity: "ana".to_owned(),
        speaking: true,
    });
    assert!(matches!(&events[..], [UiEvent::ParticipantUpdated(p)] if p.muted && p.speaking));
}

#[test]
fn avatar_events_update_participants_and_report_cache_paths() {
    let mut translator = Translator::default();
    joined(&mut translator);
    let events = translator.translate(Event::ParticipantAvatarChanged {
        identity: "ana".to_owned(),
        version: Some("0123456789abcdef".to_owned()),
    });
    assert!(matches!(
        &events[..],
        [UiEvent::ParticipantUpdated(p)]
            if p.identity == "ana" && p.avatar_version.as_deref() == Some("0123456789abcdef")
    ));
    let events = translator.translate(Event::ParticipantAvatarChanged {
        identity: "ana".to_owned(),
        version: None,
    });
    assert!(matches!(
        &events[..],
        [UiEvent::ParticipantUpdated(p)] if p.avatar_version.is_none()
    ));
    assert!(
        translator
            .translate(Event::ParticipantAvatarChanged {
                identity: "ghost".to_owned(),
                version: None,
            })
            .is_empty()
    );

    let path = std::path::PathBuf::from("avatars/ana-0123456789abcdef.png");
    assert_eq!(
        translator.translate(Event::AvatarCached {
            identity: "ana".to_owned(),
            version: "0123456789abcdef".to_owned(),
            path: path.clone(),
        }),
        vec![UiEvent::AvatarCached {
            identity: "ana".to_owned(),
            version: "0123456789abcdef".to_owned(),
            path,
        }]
    );
    assert_eq!(
        translator.translate(Event::LocalAvatarChanged { version: None }),
        vec![UiEvent::LocalAvatarChanged { version: None }]
    );
}

#[test]
fn avatar_failures_become_avatar_failed_in_portuguese() {
    let mut translator = Translator::default();
    let events = translator.translate(Event::CommandFailed(SessionError::Avatar(
        "avatar file is empty".to_owned(),
    )));
    assert!(matches!(
        &events[..],
        [UiEvent::AvatarFailed { message }] if message.contains("PNG, JPEG ou WebP")
    ));
}

#[test]
fn name_change_becomes_a_full_participant_update() {
    let mut translator = Translator::default();
    joined(&mut translator);
    let events = translator.translate(Event::ParticipantNameChanged {
        identity: "ana".to_owned(),
        name: "Ana Paula".to_owned(),
    });
    assert!(matches!(
        &events[..],
        [UiEvent::ParticipantUpdated(p)] if p.identity == "ana" && p.name == "Ana Paula"
    ));
}

#[test]
fn change_for_unknown_participant_is_dropped() {
    let mut translator = Translator::default();
    joined(&mut translator);
    let events = translator.translate(Event::ParticipantMuteChanged {
        identity: "x".to_owned(),
        muted: true,
    });
    assert!(events.is_empty());
}

#[test]
fn participant_list_is_reported_as_a_join_of_the_current_room() {
    let mut translator = Translator::default();
    joined(&mut translator);
    let events = translator.translate(Event::ParticipantList(vec![participant("pedro", "Pedro")]));
    assert!(matches!(
        &events[..],
        [UiEvent::Joined { room, local_identity, participants }]
            if room == "geral" && local_identity == "pedro" && participants.len() == 1
    ));
}

#[test]
fn chat_sender_identity_is_replaced_by_the_display_name() {
    let mut translator = Translator::default();
    joined(&mut translator);
    let events = translator.translate(Event::ChatReceived(ChatMessage {
        id: "1".to_owned(),
        sender: "ana".to_owned(),
        sent_at_unix_ms: 0,
        text: "oi".to_owned(),
        saved: false,
    }));
    assert!(matches!(
        &events[..],
        [UiEvent::ChatReceived(m)] if m.sender_id == "ana" && m.sender == "Ana"
    ));
}

#[test]
fn file_transfer_gets_display_name_and_image_flag() {
    let mut translator = Translator::default();
    joined(&mut translator);
    let transfer = |mime: &str| zenha_core::FileTransfer {
        id: "t1".to_owned(),
        sender: "ana".to_owned(),
        name: "foto.png".to_owned(),
        size: 10,
        mime: mime.to_owned(),
        outgoing: false,
        sent_at_unix_ms: 5,
        state: TransferState::InProgress { bytes: 4 },
    };
    let events = translator.translate(Event::FileTransfer(transfer("image/png")));
    assert!(matches!(
        &events[..],
        [UiEvent::FileTransfer(v)]
            if v.sender == "Ana"
                && v.is_image
                && v.state == TransferStateView::InProgress { bytes: 4 }
    ));
    let events = translator.translate(Event::FileTransfer(transfer("application/pdf")));
    assert!(matches!(&events[..], [UiEvent::FileTransfer(v)] if !v.is_image));
}

#[test]
fn send_file_command_is_forwarded() {
    let command = to_core(UiCommand::SendFile {
        path: "a.png".into(),
    });
    assert_eq!(
        command,
        Command::SendFile {
            path: "a.png".into()
        }
    );
}

#[test]
fn left_is_hidden_during_a_room_switch_and_shown_otherwise() {
    let mut translator = Translator::default();
    joined(&mut translator);
    translator.switch_pending.set(true);
    assert!(translator.translate(Event::Left).is_empty());
    assert_eq!(translator.translate(Event::Left), vec![UiEvent::Left]);
    assert!(translator.room.is_none());
}

#[test]
fn errors_are_described_in_portuguese() {
    let mut translator = Translator::default();
    assert_eq!(
        describe(&SessionError::InviteRejected),
        "Senha recusada pelo servidor."
    );
    let events = translator.translate(Event::Failed(SessionError::TokenServer("500".into())));
    assert_eq!(
        events,
        vec![UiEvent::Failed {
            message: "Não foi possível falar com o servidor de tokens: 500".to_owned()
        }]
    );
}

#[test]
fn room_lock_events_translate_to_ui_events() {
    let mut translator = Translator::default();
    assert_eq!(
        translator.translate(Event::Failed(SessionError::RoomLocked("vip".into()))),
        vec![UiEvent::RoomLocked {
            room: "vip".to_owned()
        }]
    );
    assert_eq!(
        translator.translate(Event::CommandFailed(SessionError::RoomPasswordRejected)),
        vec![UiEvent::RoomUnlockFailed {
            message: "Senha da sala incorreta.".to_owned()
        }]
    );
    let key = zenha_core::RoomKey("k".to_owned());
    assert_eq!(
        translator.translate(Event::RoomUnlocked {
            room: "vip".to_owned(),
            key: key.clone(),
        }),
        vec![UiEvent::RoomUnlocked {
            room: "vip".to_owned(),
            key
        }]
    );
}

#[test]
fn invite_rejection_and_room_refresh_errors_translate_to_ui_events() {
    let mut translator = Translator::default();
    assert_eq!(
        translator.translate(Event::Failed(SessionError::InviteRejected)),
        vec![UiEvent::InviteRejected]
    );
    assert_eq!(
        translator.translate(Event::CommandFailed(SessionError::InviteRejected)),
        vec![UiEvent::InviteRejected]
    );
    assert_eq!(
        translator.translate(Event::CommandFailed(SessionError::TokenServer(
            "offline".into()
        ))),
        vec![UiEvent::RoomsFailed {
            message: "Não foi possível falar com o servidor de tokens: offline".to_owned(),
        }]
    );
}

#[test]
fn commands_map_one_to_one() {
    assert_eq!(
        to_core(UiCommand::Join {
            room: "geral".to_owned()
        }),
        Command::Join {
            room: "geral".to_owned()
        }
    );
    assert_eq!(to_core(UiCommand::RefreshRooms), Command::RefreshRooms);
    assert_eq!(
        to_core(UiCommand::RefreshAudioDevices),
        Command::RefreshAudioDevices
    );
    assert_eq!(
        to_core(UiCommand::SetInputDevice(Some("m1".to_owned()))),
        Command::SetInputDevice(Some("m1".to_owned()))
    );
    assert_eq!(
        to_core(UiCommand::SetOutputDevice(None)),
        Command::SetOutputDevice(None)
    );
    assert_eq!(
        to_core(UiCommand::PlayCue(zenha_core::Cue::Muted)),
        Command::PlayCue(zenha_core::Cue::Muted)
    );
}

#[test]
fn share_commands_and_presets_map_one_to_one() {
    assert_eq!(
        to_core(UiCommand::StartScreenShare {
            target_id: "screen:1".to_owned(),
            preset: SharePresetView::P1080F60,
        }),
        Command::StartScreenShare {
            target_id: "screen:1".to_owned(),
            preset: SharePreset::P1080F60,
        }
    );
    assert_eq!(
        to_core(UiCommand::StopScreenShare),
        Command::StopScreenShare
    );
    assert_eq!(
        to_core(UiCommand::WatchScreenShare {
            identity: "ana".to_owned()
        }),
        Command::WatchScreenShare {
            identity: "ana".to_owned()
        }
    );
    assert_eq!(to_core(UiCommand::StopWatching), Command::StopWatching);
    assert_eq!(
        to_core(UiCommand::RefreshCaptureTargets),
        Command::RefreshCaptureTargets
    );
}

#[test]
fn share_events_are_forwarded_and_share_errors_are_shown_in_portuguese() {
    let mut translator = Translator::default();
    let events = translator.translate(Event::CaptureTargetsUpdated(vec![CaptureTarget {
        id: "window:7".to_owned(),
        name: "Bloco".to_owned(),
        kind: CaptureKind::Window,
    }]));
    assert_eq!(
        events,
        vec![UiEvent::CaptureTargetsUpdated(vec![CaptureTargetView {
            id: "window:7".to_owned(),
            name: "Bloco".to_owned(),
            kind: TargetKindView::Window,
        }])]
    );
    assert_eq!(
        translator.translate(Event::RemoteScreenShareStarted {
            identity: "ana".to_owned()
        }),
        vec![UiEvent::RemoteScreenShareStarted {
            identity: "ana".to_owned()
        }]
    );
    let events = translator.translate(Event::CommandFailed(SessionError::CaptureEnded(
        CaptureEnd::TargetClosed,
    )));
    assert!(
        matches!(&events[..], [UiEvent::ShareFailed { message }] if message.contains("fechada"))
    );
    assert_eq!(
        translator.translate(Event::MicLevel {
            level_db: -42,
            gate_open: true
        }),
        vec![UiEvent::MicLevel {
            level_db: -42,
            gate_open: true
        }]
    );
    assert_eq!(
        to_core(UiCommand::SetNoiseGate(Some(-45))),
        Command::SetNoiseGate(Some(-45))
    );
    assert!(
        translator
            .translate(Event::CommandFailed(SessionError::NotInRoom))
            .is_empty()
    );
}

#[test]
fn watching_started_carries_a_feed_that_yields_each_frame_once() {
    let mut translator = Translator::default();
    let slot = FrameSlot::new();
    let events = translator.translate(Event::CaptureThumbnail {
        id: "window:7".to_owned(),
        width: 2,
        height: 1,
        rgba: vec![9; 8],
    });
    assert_eq!(
        events,
        vec![UiEvent::CaptureThumbnail {
            id: "window:7".to_owned(),
            width: 2,
            height: 1,
            rgba: vec![9; 8],
        }]
    );
    let preview_slot = FrameSlot::new();
    preview_slot.push(VideoFrame {
        width: 2,
        height: 2,
        rgba: vec![5; 16],
    });
    let events = translator.translate(Event::ScreenShareStarted {
        preview: preview_slot,
    });
    let [UiEvent::ScreenShareStarted { preview }] = &events[..] else {
        panic!("expected ScreenShareStarted");
    };
    let mut preview_seen = 0;
    let preview_frame = preview.take_newer(&mut preview_seen).unwrap();
    assert_eq!((preview_frame.width, preview_frame.height), (2, 2));
    assert_eq!(preview_frame.bytes(), &[5; 16]);
    let events = translator.translate(Event::WatchingStarted {
        identity: "ana".to_owned(),
        frames: slot.clone(),
    });
    let [UiEvent::WatchingStarted { identity, feed }] = &events[..] else {
        panic!("unexpected {events:?}");
    };
    assert_eq!(identity, "ana");
    let mut seen = 0;
    assert!(feed.take_newer(&mut seen).is_none());
    slot.push(VideoFrame {
        width: 1,
        height: 1,
        rgba: vec![1, 2, 3, 255],
    });
    let frame = feed.take_newer(&mut seen).unwrap();
    assert_eq!((frame.width, frame.height), (1, 1));
    assert_eq!(frame.bytes(), &[1, 2, 3, 255]);
    assert!(feed.take_newer(&mut seen).is_none());
}

#[test]
fn test_feed_produces_frames() {
    let feed = TestFeed::start().unwrap();
    let handle = feed.handle();
    let mut seen = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut frame = None;
    while frame.is_none() && std::time::Instant::now() < deadline {
        frame = handle.take_newer(&mut seen);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let frame = frame.unwrap();
    assert_eq!((frame.width, frame.height), (1280, 720));
    assert_eq!(frame.bytes().len(), 1280 * 720 * 4);
}

#[test]
fn audio_devices_become_plain_ui_structs() {
    let mut translator = Translator::default();
    let events = translator.translate(Event::AudioDevicesUpdated(zenha_core::AudioDevices {
        inputs: vec![AudioDevice {
            id: "m1".to_owned(),
            name: "Mic".to_owned(),
        }],
        outputs: Vec::new(),
        selected_input: Some("m1".to_owned()),
        selected_output: None,
    }));
    assert_eq!(
        events,
        vec![UiEvent::AudioDevicesUpdated {
            inputs: vec![AudioDeviceView {
                id: "m1".to_owned(),
                name: "Mic".to_owned()
            }],
            outputs: Vec::new(),
            selected_input: Some("m1".to_owned()),
            selected_output: None,
        }]
    );
}

#[test]
fn chat_commands_map_with_save_history_and_delete() {
    assert_eq!(
        to_core(UiCommand::SendChat {
            text: "oi".to_owned(),
            save: true,
        }),
        Command::SendChat {
            text: "oi".to_owned(),
            save: true,
        }
    );
    assert_eq!(
        to_core(UiCommand::LoadChatHistory {
            before: Some("m1".to_owned()),
        }),
        Command::LoadChatHistory {
            before: Some("m1".to_owned()),
        }
    );
    assert_eq!(
        to_core(UiCommand::DeleteChat {
            id: "m2".to_owned(),
        }),
        Command::DeleteChat {
            id: "m2".to_owned(),
        }
    );
}

#[test]
fn chat_history_and_delete_events_keep_sender_identity_and_name() {
    let mut translator = Translator::default();
    joined(&mut translator);
    let events = translator.translate(Event::ChatHistory {
        messages: vec![SavedChatMessage {
            message: ChatMessage {
                id: "m1".to_owned(),
                sender: "Ana Silva".to_owned(),
                sent_at_unix_ms: 1000,
                text: "salva".to_owned(),
                saved: true,
            },
            sender_id: "ana".to_owned(),
        }],
        has_more: true,
        before: Some("m2".to_owned()),
        webhook_posts: Vec::new(),
    });
    assert_eq!(
        events,
        vec![UiEvent::ChatHistory {
            messages: vec![ChatMessageView {
                id: "m1".to_owned(),
                sender_id: "ana".to_owned(),
                sender: "Ana Silva".to_owned(),
                sent_at_unix_ms: 1000,
                text: "salva".to_owned(),
                saved: true,
            }],
            has_more: true,
            before: Some("m2".to_owned()),
            webhook_posts: Vec::new(),
        }]
    );

    let events = translator.translate(Event::ChatDeleted {
        id: "m1".to_owned(),
        sender: "ana".to_owned(),
    });
    assert_eq!(
        events,
        vec![UiEvent::ChatDeleted {
            id: "m1".to_owned(),
            sender: "ana".to_owned(),
        }]
    );
}

#[test]
fn chat_command_failures_translate_to_portuguese_notices() {
    let mut translator = Translator::default();
    joined(&mut translator);
    assert_eq!(
        translator.translate(Event::CommandFailed(SessionError::RateLimited)),
        vec![UiEvent::ChatFailed {
            message: "Muitas mensagens seguidas. Espere alguns segundos.".to_owned(),
        }]
    );
    assert_eq!(
        translator.translate(Event::CommandFailed(SessionError::Unauthorized)),
        vec![UiEvent::ChatFailed {
            message: "Não foi possível salvar: sessão expirada. Tente de novo.".to_owned(),
        }]
    );
    assert!(matches!(
        &translator.translate(Event::CommandFailed(SessionError::Chat("503".into())))[..],
        [UiEvent::ChatFailed { .. }]
    ));
    assert_eq!(
        translator.translate(Event::CommandFailed(SessionError::TokenServer(
            "offline".into()
        ))),
        vec![UiEvent::RoomsFailed {
            message: "Não foi possível falar com o servidor de tokens: offline".to_owned(),
        }]
    );
}

#[test]
fn participant_volume_ui_command_translates_to_core() {
    assert_eq!(
        to_core(UiCommand::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 135,
        }),
        Command::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 135,
        }
    );
}

#[test]
fn cancel_capture_targets_and_ui_visibility_translate_to_core() {
    assert_eq!(
        to_core(UiCommand::CancelCaptureTargets),
        Command::CancelCaptureTargets
    );
    assert_eq!(
        to_core(UiCommand::SetUiVisible(false)),
        Command::SetUiVisible(false)
    );
    assert_eq!(
        to_core(UiCommand::SetUiVisible(true)),
        Command::SetUiVisible(true)
    );
}

#[test]
fn admin_commands_translate_to_core() {
    assert_eq!(
        to_core(UiCommand::AdminLogin {
            password: "pw".to_owned(),
        }),
        Command::AdminLogin {
            password: "pw".to_owned(),
        }
    );
    assert_eq!(to_core(UiCommand::AdminLogout), Command::AdminLogout);

    let action = zenha_core::AdminAction::MuteParticipant {
        room: "sala-1".to_owned(),
        identity: "bob".to_owned(),
    };
    assert_eq!(
        to_core(UiCommand::Admin(action.clone())),
        Command::Admin(action)
    );
}

#[test]
fn admin_errors_translate_to_portuguese_admin_failed() {
    let mut translator = Translator::default();
    let cases = [
        (
            SessionError::AdminInvalidPassword,
            "Senha de administrador incorreta.",
        ),
        (
            SessionError::AdminUnauthorized,
            "Sessão de administrador expirada. Entre novamente.",
        ),
        (
            SessionError::AdminDisabled,
            "O modo administrador não está habilitado neste servidor.",
        ),
        (
            SessionError::AdminForbidden,
            "Sem permissão de administrador.",
        ),
        (
            SessionError::AdminRateLimited,
            "Muitas tentativas de login de administrador. Aguarde antes de tentar novamente.",
        ),
        (
            SessionError::AdminTargetNotFound("bob".to_owned()),
            "Sala, participante ou mensagem não encontrado no servidor.",
        ),
        (
            SessionError::Admin("cannot delete the last room".to_owned()),
            "Não é possível apagar a última sala.",
        ),
        (
            SessionError::Admin("erro qualquer".to_owned()),
            "Falha na ação de administrador: erro qualquer",
        ),
    ];

    for (err, expected) in cases {
        assert_eq!(
            translator.translate(Event::CommandFailed(err)),
            vec![UiEvent::AdminFailed {
                message: expected.to_owned(),
            }]
        );
    }
}

#[test]
fn moved_by_admin_suppresses_intermediate_left_and_emits_notice_event() {
    let mut translator = Translator::default();
    joined(&mut translator);

    assert_eq!(
        translator.translate(Event::MovedByAdmin {
            to_room: "sala-2".to_owned(),
        }),
        vec![UiEvent::MovedByAdmin {
            to_room: "sala-2".to_owned(),
        }]
    );
    assert!(translator.translate(Event::Left).is_empty());
    assert_eq!(
        translator.translate(Event::Connecting),
        vec![UiEvent::Connecting]
    );
}

#[test]
fn removed_from_room_resets_translator_even_during_switch() {
    let mut translator = Translator::default();
    joined(&mut translator);
    translator.switch_pending.set(true);

    let events = translator.translate(Event::RemovedFromRoom {
        reason: zenha_core::RemovalReason::Kicked,
    });
    assert_eq!(
        events,
        vec![UiEvent::RemovedFromRoom {
            reason: zenha_core::RemovalReason::Kicked,
        }]
    );
    assert!(translator.room.is_none());
    assert!(translator.participants.is_empty());
    assert!(!translator.switch_pending.get());
}

#[test]
fn admin_events_translate_cleanly() {
    let mut translator = Translator::default();
    let room = zenha_protocol::AdminRoom {
        id: "sala-2".to_owned(),
        title: "Sala Dois".to_owned(),
        position: 1,
        locked: false,
        parent: None,
    };
    assert_eq!(
        translator.translate(Event::AdminChanged { active: true }),
        vec![UiEvent::AdminChanged { active: true }]
    );
    assert_eq!(
        translator.translate(Event::AdminRoomCreated(room.clone())),
        vec![UiEvent::AdminRoomCreated(room.clone())]
    );
    assert_eq!(
        translator.translate(Event::AdminRoomUpdated(room.clone())),
        vec![UiEvent::AdminRoomUpdated(room)]
    );
    assert_eq!(
        translator.translate(Event::AdminRoomDeleted {
            id: "sala-2".to_owned(),
        }),
        vec![UiEvent::AdminRoomDeleted {
            id: "sala-2".to_owned(),
        }]
    );
    assert_eq!(
        translator.translate(Event::AdminInviteRotated),
        vec![UiEvent::AdminInviteRotated]
    );
    assert_eq!(
        translator.translate(Event::AdminStoppedScreenShare),
        vec![UiEvent::AdminStoppedScreenShare]
    );
}

#[test]
fn webhook_events_translate_cleanly() {
    let mut translator = Translator::default();
    let post = zenha_protocol::WebhookPost {
        id: "abc".to_owned(),
        name: "Gamers Club".to_owned(),
        sent_at_unix_ms: 1,
        content: Some("oi".to_owned()),
        embeds: Vec::new(),
    };
    assert_eq!(
        translator.translate(Event::WebhookPosted(post.clone())),
        vec![UiEvent::WebhookPosted(post)]
    );
    let webhook = zenha_protocol::AdminWebhook {
        id: "abc".to_owned(),
        room: "geral".to_owned(),
        name: "Gamers Club".to_owned(),
        created_at_unix_ms: 2,
    };
    assert_eq!(
        translator.translate(Event::AdminWebhooks(vec![webhook.clone()])),
        vec![UiEvent::AdminWebhooks(vec![webhook.clone()])]
    );
    let created = zenha_protocol::CreatedWebhook {
        webhook,
        url: "https://voz.exemplo/hooks/abc/tok".to_owned(),
    };
    assert_eq!(
        translator.translate(Event::AdminWebhookCreated(created.clone())),
        vec![UiEvent::AdminWebhookCreated(created)]
    );
    assert_eq!(
        translator.translate(Event::AdminWebhookDeleted {
            id: "abc".to_owned(),
        }),
        vec![UiEvent::AdminWebhookDeleted {
            id: "abc".to_owned(),
        }]
    );
}
