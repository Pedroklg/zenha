//! Tests for the demo session.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use zenha_core::AdminAction;

use crate::ui_model::TransferStateView;

use super::*;

fn drain(session: &mut DemoSession) -> Vec<UiEvent> {
    std::iter::from_fn(|| session.try_recv()).collect()
}

fn joined(session: &mut DemoSession) -> Vec<UiEvent> {
    session.send(UiCommand::Join {
        room: "geral".to_owned(),
    });
    drain(session)
}

#[test]
fn join_yields_connecting_then_joined() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    let events = joined(&mut session);
    assert_eq!(events.first(), Some(&UiEvent::Connecting));
    assert!(events.iter().any(|e| matches!(
        e,
        UiEvent::Joined { room, local_identity, participants }
            if room == "geral" && local_identity == SELF_IDENTITY && participants.len() == 2
    )));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::ParticipantJoined(p) if p.name == "Bruno"))
    );
}

#[test]
fn joining_the_failing_room_reports_failure() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::Join {
        room: FAILING_ROOM.to_owned(),
    });
    let events = drain(&mut session);
    assert!(matches!(events.last(), Some(UiEvent::Failed { .. })));
}

#[test]
fn join_is_delayed_with_default_delays() {
    let mut session = DemoSession::new();
    session.send(UiCommand::Join {
        room: "geral".to_owned(),
    });
    let events = drain(&mut session);
    assert_eq!(events, vec![UiEvent::Connecting]);
}

#[test]
fn chat_is_echoed_and_every_second_message_gets_a_reply() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    joined(&mut session);
    session.send(UiCommand::SendChat {
        text: "oi".to_owned(),
        save: false,
    });
    let first = drain(&mut session);
    assert!(matches!(
        first.as_slice(),
        [UiEvent::ChatReceived(m)] if m.text == "oi" && m.sender == SELF_NAME && !m.saved
    ));
    session.send(UiCommand::SendChat {
        text: "tudo bem?".to_owned(),
        save: false,
    });
    let second = drain(&mut session);
    assert_eq!(second.len(), 2);
    assert!(matches!(&second[1], UiEvent::ChatReceived(m) if m.sender == "Ana"));
}

#[test]
fn sent_file_goes_from_progress_to_done() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    joined(&mut session);
    session.send(UiCommand::SendFile {
        path: PathBuf::from("/nonexistent/foto.png"),
    });
    let events = drain(&mut session);
    assert!(matches!(
        events.as_slice(),
        [
            UiEvent::FileTransfer(a),
            UiEvent::FileTransfer(b),
        ] if a.id == b.id
            && a.is_image
            && a.outgoing
            && matches!(a.state, TransferStateView::InProgress { .. })
            && matches!(&b.state, TransferStateView::Done { path } if path.ends_with("foto.png"))
    ));
}

#[test]
fn file_outside_a_room_is_ignored() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::SendFile {
        path: PathBuf::from("a.png"),
    });
    assert!(drain(&mut session).is_empty());
}

#[test]
fn chat_outside_a_room_is_ignored() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::SendChat {
        text: "oi".to_owned(),
        save: false,
    });
    assert!(drain(&mut session).is_empty());
}

#[test]
fn refresh_returns_all_rooms_with_live_occupants() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    joined(&mut session);
    session.send(UiCommand::RefreshRooms);
    let events = drain(&mut session);
    let Some(UiEvent::RoomsUpdated(rooms)) = events.first() else {
        panic!("expected RoomsUpdated, got {events:?}");
    };
    assert_eq!(rooms.len(), 5);
    let geral = &rooms[0];
    assert!(geral.participants.contains(&SELF_NAME.to_owned()));
}

#[test]
fn set_muted_updates_own_participant() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    joined(&mut session);
    session.send(UiCommand::SetMuted(true));
    let events = drain(&mut session);
    assert!(matches!(
        events.as_slice(),
        [UiEvent::ParticipantUpdated(p)] if p.identity == SELF_IDENTITY && p.muted
    ));
}

#[test]
fn set_display_name_updates_own_participant() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    joined(&mut session);
    session.send(UiCommand::SetDisplayName {
        name: "Lia".to_owned(),
    });
    let events = drain(&mut session);
    assert!(matches!(
        events.as_slice(),
        [UiEvent::ParticipantUpdated(p)] if p.identity == SELF_IDENTITY && p.name == "Lia"
    ));
}

#[test]
fn audio_devices_are_listed_and_selection_is_echoed() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::RefreshAudioDevices);
    let events = drain(&mut session);
    let [
        UiEvent::AudioDevicesUpdated {
            inputs,
            outputs,
            selected_input: None,
            selected_output: None,
        },
    ] = events.as_slice()
    else {
        panic!("unexpected events: {events:?}");
    };
    assert_eq!(inputs.len(), 2);
    assert_eq!(outputs.len(), 2);

    session.send(UiCommand::SetOutputDevice(Some("demo-out-1".to_owned())));
    let events = drain(&mut session);
    assert!(matches!(
        events.as_slice(),
        [UiEvent::AudioDevicesUpdated { selected_output: Some(id), .. }] if id == "demo-out-1"
    ));
}

#[test]
fn capture_targets_are_listed() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::RefreshCaptureTargets);
    let events = drain(&mut session);
    assert!(matches!(
        events.first(),
        Some(UiEvent::CaptureTargetsUpdated(t)) if t.len() == 4
    ));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, UiEvent::CaptureThumbnail { .. }))
            .count(),
        4
    );
}

#[test]
fn local_share_starts_stops_and_rejects_unknown_targets() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    joined(&mut session);
    let start = |id: &str| UiCommand::StartScreenShare {
        target_id: id.to_owned(),
        preset: crate::ui_model::SharePresetView::default(),
    };
    session.send(start("nope"));
    assert!(matches!(
        drain(&mut session).as_slice(),
        [UiEvent::ShareFailed { .. }]
    ));
    session.send(start("demo:screen:0"));
    let events = drain(&mut session);
    let [UiEvent::ScreenShareStarted { preview }] = events.as_slice() else {
        panic!("unexpected events: {events:?}");
    };
    let mut seen = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while preview.take_newer(&mut seen).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(seen > 0);
    session.send(start("demo:screen:1"));
    assert!(drain(&mut session).is_empty());
    session.send(UiCommand::StopScreenShare);
    assert_eq!(drain(&mut session), vec![UiEvent::ScreenShareStopped]);
}

#[test]
fn ana_shares_and_can_be_watched_until_stop_or_leave() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    let events = joined(&mut session);
    assert!(events.iter().any(|e| matches!(
        e,
        UiEvent::RemoteScreenShareStarted { identity } if identity == "demo-ana"
    )));
    session.send(UiCommand::WatchScreenShare {
        identity: "demo-ana".to_owned(),
    });
    let events = drain(&mut session);
    let [UiEvent::WatchingStarted { identity, feed }] = events.as_slice() else {
        panic!("unexpected events: {events:?}");
    };
    assert_eq!(identity, "demo-ana");
    let mut seen = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while feed.take_newer(&mut seen).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(seen > 0);
    session.send(UiCommand::StopWatching);
    assert_eq!(drain(&mut session), vec![UiEvent::WatchingStopped]);
    session.send(UiCommand::WatchScreenShare {
        identity: "demo-bruno".to_owned(),
    });
    assert!(drain(&mut session).is_empty());
}

#[test]
fn leave_emits_left_and_drops_pending_events() {
    let mut session = DemoSession::with_delays(DemoDelays::default());
    session.send(UiCommand::Join {
        room: "geral".to_owned(),
    });
    session.send(UiCommand::Leave);
    assert_eq!(drain(&mut session), vec![UiEvent::Left]);
}

#[test]
fn demo_supports_saved_messages_history_pages_and_deletion() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    let events = joined(&mut session);
    assert!(events.iter().any(|e| matches!(
        e,
        UiEvent::ChatHistory {
            messages,
            has_more: true,
            before: None,
        } if !messages.is_empty() && messages.iter().all(|m| m.saved)
    )));

    session.send(UiCommand::SendChat {
        text: "salva".to_owned(),
        save: true,
    });
    assert!(matches!(
        drain(&mut session).as_slice(),
        [UiEvent::ChatReceived(m)] if m.saved && m.sender_id == SELF_IDENTITY && m.text == "salva"
    ));

    session.send(UiCommand::LoadChatHistory {
        before: Some("demo-hist-3".to_owned()),
    });
    assert!(matches!(
        drain(&mut session).as_slice(),
        [UiEvent::ChatHistory {
            messages,
            has_more: false,
            before: Some(cursor),
        }] if !messages.is_empty() && cursor == "demo-hist-3"
    ));

    session.send(UiCommand::DeleteChat {
        id: "demo-hist-4".to_owned(),
    });
    assert_eq!(
        drain(&mut session),
        vec![UiEvent::ChatDeleted {
            id: "demo-hist-4".to_owned(),
            sender: SELF_IDENTITY.to_owned(),
        }]
    );
}

#[test]
fn participant_volume_command_is_accepted_without_effect() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::SetParticipantVolume {
        identity: "demo-ana".to_owned(),
        percent: 60,
    });
    assert!(drain(&mut session).is_empty());
}

#[test]
fn cancel_capture_targets_drops_scheduled_thumbnails_and_ui_visible_is_accepted() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::RefreshCaptureTargets);
    session.send(UiCommand::CancelCaptureTargets);
    session.send(UiCommand::SetUiVisible(false));
    session.send(UiCommand::SetUiVisible(true));
    assert!(drain(&mut session).is_empty());
}

#[test]
fn demo_admin_commands_emit_simulated_events() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::AdminLogin {
        password: "admin".to_owned(),
    });
    assert_eq!(
        drain(&mut session),
        vec![UiEvent::AdminChanged { active: true }]
    );

    session.send(UiCommand::Admin(AdminAction::CreateRoom {
        title: "Estudos".to_owned(),
        password: None,
        parent: None,
    }));
    let events = drain(&mut session);
    assert!(events.iter().any(|e| matches!(
        e,
        UiEvent::AdminRoomCreated(room) if room.title == "Estudos"
    )));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::RoomsUpdated(rooms) if rooms.len() == 6))
    );

    session.send(UiCommand::Admin(AdminAction::RotateInvite {
        new_code: "novo-codigo-123".to_owned(),
    }));
    assert_eq!(drain(&mut session), vec![UiEvent::AdminInviteRotated]);

    session.send(UiCommand::Admin(AdminAction::DeleteChat {
        room: "geral".to_owned(),
        id: "demo-hist-1".to_owned(),
    }));
    assert_eq!(
        drain(&mut session),
        vec![UiEvent::ChatDeleted {
            id: "demo-hist-1".to_owned(),
            sender: String::new(),
        }]
    );

    session.send(UiCommand::AdminLogout);
    assert_eq!(
        drain(&mut session),
        vec![UiEvent::AdminChanged { active: false }]
    );
}

#[test]
fn demo_mic_reading_varies_and_toggles_gate_with_threshold() {
    let off: Vec<_> = (0..DEMO_MIC_LEVELS.len() as u64)
        .map(|step| demo_mic_reading(step, None))
        .collect();
    assert!(off.iter().all(|(_, open)| *open));
    let min = off.iter().map(|(db, _)| *db).min().unwrap();
    let max = off.iter().map(|(db, _)| *db).max().unwrap();
    assert!(min < -45 && max > -45);

    let gated: Vec<_> = (0..DEMO_MIC_LEVELS.len() as u64)
        .map(|step| demo_mic_reading(step, Some(-45)))
        .collect();
    assert!(gated.iter().any(|(_, open)| !*open));
    assert!(gated.iter().any(|(_, open)| *open));
}

#[test]
fn demo_session_emits_mic_levels_while_in_room_and_stops_on_leave() {
    let mut session = DemoSession::with_delays(DemoDelays {
        mic_level: Duration::from_millis(1),
        ..DemoDelays::instant()
    });
    session.send(UiCommand::SetNoiseGate(Some(-45)));
    session.send(UiCommand::Join {
        room: "geral".to_owned(),
    });
    let initial = drain(&mut session);
    assert!(initial.iter().any(|e| matches!(e, UiEvent::Joined { .. })));

    let mut levels = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while levels.len() < 3 && Instant::now() < deadline {
        for event in drain(&mut session) {
            if let UiEvent::MicLevel {
                level_db,
                gate_open,
            } = event
            {
                levels.push((level_db, gate_open));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(levels.len() >= 3);
    assert!(levels.iter().all(|(db, open)| *open == (*db >= -45)));

    session.send(UiCommand::Leave);
    assert!(
        drain(&mut session)
            .iter()
            .any(|e| matches!(e, UiEvent::Left))
    );
    std::thread::sleep(Duration::from_millis(5));
    assert!(drain(&mut session).is_empty());
}

#[test]
fn locked_room_and_sub_room_need_the_password_once() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::RefreshRooms);
    let rooms = drain(&mut session)
        .into_iter()
        .find_map(|e| match e {
            UiEvent::RoomsUpdated(rooms) => Some(rooms),
            _ => None,
        })
        .unwrap();
    let vip = rooms.iter().find(|r| r.name == "vip").unwrap();
    assert!(vip.needs_password() && vip.participants.is_empty());
    assert_eq!(vip.occupant_count(), 1);
    let child = rooms.iter().find(|r| r.name == "cantinho").unwrap();
    assert!(child.needs_password());
    assert_eq!(child.parent.as_deref(), Some("vip"));

    session.send(UiCommand::Join {
        room: "cantinho".to_owned(),
    });
    assert!(drain(&mut session).contains(&UiEvent::RoomLocked {
        room: "cantinho".to_owned()
    }));

    session.send(UiCommand::UnlockRoom {
        room: "cantinho".to_owned(),
        password: "errada".to_owned(),
    });
    assert!(matches!(
        drain(&mut session)[..],
        [UiEvent::RoomUnlockFailed { .. }]
    ));

    session.send(UiCommand::UnlockRoom {
        room: "cantinho".to_owned(),
        password: DEMO_ROOM_PASSWORD.to_owned(),
    });
    let events = drain(&mut session);
    assert!(matches!(&events[0], UiEvent::RoomUnlocked { room, .. } if room == "vip"));
    session.send(UiCommand::Join {
        room: "cantinho".to_owned(),
    });
    assert!(
        drain(&mut session)
            .iter()
            .any(|e| matches!(e, UiEvent::Joined { room, .. } if room == "cantinho"))
    );
}

#[test]
fn join_posts_a_lobby_card_and_a_match_card() {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    let events = joined(&mut session);
    let posts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            UiEvent::WebhookPosted(post) => Some(post),
            _ => None,
        })
        .collect();
    assert_eq!(posts.len(), 2);
    assert_eq!(
        posts[0].embeds[0].url.as_deref(),
        Some("https://gamersclub.com.br/j/123/abc")
    );
    assert!(
        posts[1].embeds[0].fields[0]
            .value
            .starts_with("connect 1.2.3.4:27015;password xyz")
    );
    assert!(posts.iter().all(|post| post.validate().is_ok()));
}

fn admin_session() -> DemoSession {
    let mut session = DemoSession::with_delays(DemoDelays::instant());
    session.send(UiCommand::AdminLogin {
        password: "x".to_owned(),
    });
    drain(&mut session);
    session
}

#[test]
fn admin_lists_two_fake_webhooks() {
    let mut session = admin_session();
    session.send(UiCommand::Admin(AdminAction::ListWebhooks));
    let events = drain(&mut session);
    assert!(matches!(
        events.as_slice(),
        [UiEvent::AdminWebhooks(list)] if list.len() == 2
            && list[0].room == "geral"
            && list[1].room == "jogos"
    ));
}

#[test]
fn admin_creates_then_revokes_a_webhook() {
    let mut session = admin_session();
    session.send(UiCommand::Admin(AdminAction::CreateWebhook {
        room: "geral".to_owned(),
        name: "Novo".to_owned(),
    }));
    let events = drain(&mut session);
    let Some(UiEvent::AdminWebhookCreated(created)) = events.first() else {
        panic!("expected AdminWebhookCreated, got {events:?}");
    };
    assert_eq!(created.webhook.name, "Novo");
    assert!(created.url.starts_with("https://demo.zenha.local/hooks/"));

    let id = created.webhook.id.clone();
    session.send(UiCommand::Admin(AdminAction::DeleteWebhook { id: id.clone() }));
    assert_eq!(drain(&mut session), vec![UiEvent::AdminWebhookDeleted { id }]);

    session.send(UiCommand::Admin(AdminAction::ListWebhooks));
    assert!(matches!(
        drain(&mut session).as_slice(),
        [UiEvent::AdminWebhooks(list)] if list.len() == 2
    ));
}

#[test]
fn webhook_admin_rejects_unknown_room_and_unknown_id() {
    let mut session = admin_session();
    session.send(UiCommand::Admin(AdminAction::CreateWebhook {
        room: "nao-existe".to_owned(),
        name: "x".to_owned(),
    }));
    session.send(UiCommand::Admin(AdminAction::DeleteWebhook {
        id: "nao-existe".to_owned(),
    }));
    let events = drain(&mut session);
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| matches!(e, UiEvent::AdminFailed { .. })));
}
