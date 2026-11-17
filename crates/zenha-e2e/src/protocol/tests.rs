//! Testes do protocolo de comandos e eventos.

use zenha_core::{
    AdminAction, Command, Cue, Event, FrameSlot, RemovalReason, SessionError,
    TEST_PATTERN_TARGET_ID, TransferState,
};
use zenha_protocol::ChatMessage;

use super::*;

#[test]
fn commands_parse_with_snake_and_kebab_names() {
    assert_eq!(
        parse_command(r#"{"cmd":"join","room":"x"}"#),
        Ok(BotCommand::Join { room: "x".into() })
    );
    assert_eq!(
        parse_command(r#"{"cmd":"set_muted","muted":true}"#),
        Ok(BotCommand::SetMuted { muted: true })
    );
    assert_eq!(
        parse_command(r#"{"cmd":"set-deafened","deafened":false}"#),
        Ok(BotCommand::SetDeafened { deafened: false })
    );
    assert_eq!(
        parse_command(r#"{"cmd":"set_output_device","id":null}"#),
        Ok(BotCommand::SetOutputDevice { id: None })
    );
    assert_eq!(
        parse_command(r#" {"cmd":"measure-frames","duration_ms":3000} "#),
        Ok(BotCommand::MeasureFrames { duration_ms: 3000 })
    );
    assert_eq!(
        parse_command(r#"{"cmd":"play_cue","cue":"muted"}"#),
        Ok(BotCommand::PlayCue {
            cue: "muted".into()
        })
    );
    assert_eq!(
        parse_command(r#"{"cmd":"play-cue","cue":"undeafened"}"#),
        Ok(BotCommand::PlayCue {
            cue: "undeafened".into()
        })
    );
    assert!(parse_command(r#"{"cmd":"play_cue","cue":"bogus"}"#).is_err());
    assert_eq!(parse_command(r#"{"cmd":"quit"}"#), Ok(BotCommand::Quit));
    assert!(parse_command(r#"{"cmd":"explode"}"#).is_err());
    assert!(parse_command("not json").is_err());
}

#[test]
fn every_command_round_trips() {
    let all = [
        BotCommand::Join { room: "r".into() },
        BotCommand::Leave,
        BotCommand::SetMuted { muted: true },
        BotCommand::SetDeafened { deafened: true },
        BotCommand::SendChat { text: "oi".into() },
        BotCommand::SetOutputDevice {
            id: Some("d".into()),
        },
        BotCommand::SetInputDevice { id: None },
        BotCommand::RefreshDevices,
        BotCommand::StartShare,
        BotCommand::StopShare,
        BotCommand::Watch {
            identity: "t".into(),
        },
        BotCommand::StopWatching,
        BotCommand::CancelCaptureTargets,
        BotCommand::SetUiVisible { visible: false },
        BotCommand::MeasureFrames { duration_ms: 10 },
        BotCommand::PlayCue {
            cue: "muted".into(),
        },
        BotCommand::PlayCue {
            cue: "unmuted".into(),
        },
        BotCommand::PlayCue {
            cue: "deafened".into(),
        },
        BotCommand::PlayCue {
            cue: "undeafened".into(),
        },
        BotCommand::Quit,
    ];
    for command in all {
        let line = serde_json::to_string(&command).unwrap();
        assert_eq!(parse_command(&line), Ok(command));
    }
}

#[test]
fn session_commands_map_and_local_ones_do_not() {
    assert_eq!(
        BotCommand::SetMuted { muted: true }.to_core(),
        Some(Command::SetMuted(true))
    );
    for (name, expected) in [
        ("muted", Cue::Muted),
        ("unmuted", Cue::Unmuted),
        ("deafened", Cue::Deafened),
        ("undeafened", Cue::Undeafened),
        ("self_joined", Cue::SelfJoined),
        ("self_left", Cue::SelfLeft),
        ("peer_joined", Cue::PeerJoined),
        ("peer_left", Cue::PeerLeft),
    ] {
        assert_eq!(
            BotCommand::PlayCue { cue: name.into() }.to_core(),
            Some(Command::PlayCue(expected))
        );
    }
    assert_eq!(BotCommand::PlayCue { cue: "nope".into() }.to_core(), None);
    assert!(matches!(
        BotCommand::StartShare.to_core(),
        Some(Command::StartScreenShare { target_id, .. }) if target_id == TEST_PATTERN_TARGET_ID
    ));
    assert_eq!(
        BotCommand::CancelCaptureTargets.to_core(),
        Some(Command::CancelCaptureTargets)
    );
    assert_eq!(
        BotCommand::SetUiVisible { visible: false }.to_core(),
        Some(Command::SetUiVisible(false))
    );
    assert_eq!(BotCommand::Quit.to_core(), None);
    assert_eq!(BotCommand::MeasureFrames { duration_ms: 1 }.to_core(), None);
}

#[test]
fn events_round_trip_through_a_stamped_line() {
    let events = [
        BotEvent::Left,
        BotEvent::Joined {
            room: "r".into(),
            local_identity: "a".into(),
            participants: vec![ParticipantInfo {
                identity: "a".into(),
                name: "A".into(),
                muted: false,
                speaking: true,
            }],
        },
        BotEvent::Failed {
            error: ErrorInfo {
                kind: "invite_rejected".into(),
                message: "invite code rejected".into(),
            },
        },
        BotEvent::FrameMeasurement {
            identity: "t".into(),
            frames: 90,
            seconds: 3.0,
            fps: 30.0,
            width: 640,
            height: 360,
            mean_rgb: [1.0, 2.0, 3.0],
            luma_stddev: 4.0,
            gap_p95_ms: 17.0,
            gap_max_ms: 34.0,
            stutters: 1,
        },
        BotEvent::Bye,
    ];
    for event in events {
        let line = encode_event(1234, &event);
        assert!(!line.contains('\n'));
        let decoded = decode_event(&line).unwrap();
        assert_eq!(decoded.t_ms, 1234);
        assert_eq!(decoded.event, event);
    }
    assert!(decode_event("some native log line").is_none());
}

#[test]
fn core_events_are_translated() {
    let chat = Event::ChatReceived(ChatMessage {
        id: "1".into(),
        sender: "t".into(),
        sent_at_unix_ms: 9,
        text: "oi".into(),
        saved: false,
    });
    assert_eq!(
        BotEvent::from_core(&chat),
        Some(BotEvent::ChatReceived {
            id: "1".into(),
            sender: "t".into(),
            text: "oi".into(),
            sent_at_unix_ms: 9
        })
    );
    let failed = Event::Failed(SessionError::InviteRejected);
    assert_eq!(
        BotEvent::from_core(&failed),
        Some(BotEvent::Failed {
            error: ErrorInfo {
                kind: "invite_rejected".into(),
                message: "invite code rejected".into()
            }
        })
    );
    let watching = Event::WatchingStarted {
        identity: "t".into(),
        frames: FrameSlot::new(),
    };
    assert_eq!(
        BotEvent::from_core(&watching),
        Some(BotEvent::WatchingStarted {
            identity: "t".into()
        })
    );
}

fn transfer(state: TransferState) -> Event {
    Event::FileTransfer(zenha_core::FileTransfer {
        id: "f1".into(),
        sender: "alice".into(),
        name: "a.png".into(),
        size: 10,
        mime: "image/png".into(),
        outgoing: false,
        sent_at_unix_ms: 1,
        state,
    })
}

#[test]
fn file_transfer_events_log_start_and_end_only() {
    assert_eq!(
        parse_command(r#"{"cmd":"send-file","path":"/tmp/x"}"#),
        Ok(BotCommand::SendFile {
            path: "/tmp/x".into()
        })
    );
    assert_eq!(
        BotCommand::SendFile {
            path: "/tmp/x".into()
        }
        .to_core(),
        Some(Command::SendFile {
            path: "/tmp/x".into()
        })
    );
    let started = BotEvent::from_core(&transfer(TransferState::InProgress { bytes: 0 }));
    assert!(matches!(
        started,
        Some(BotEvent::FileTransfer { ref state, bytes: Some(0), .. }) if state == "in_progress"
    ));
    assert_eq!(
        BotEvent::from_core(&transfer(TransferState::InProgress { bytes: 5 })),
        None
    );
    let done = BotEvent::from_core(&transfer(TransferState::Done {
        path: "/r/f1-a.png".into(),
    }));
    assert!(matches!(
        done,
        Some(BotEvent::FileTransfer { ref state, path: Some(ref path), .. })
            if state == "done" && path == "/r/f1-a.png"
    ));
    let failed = BotEvent::from_core(&transfer(TransferState::Failed("x".into())));
    assert!(matches!(
        failed,
        Some(BotEvent::FileTransfer { ref state, error: Some(ref error), .. })
            if state == "failed" && error == "x"
    ));
}

#[test]
fn mic_level_events_are_not_logged_and_noise_gate_commands_parse() {
    let level = Event::MicLevel {
        level_db: -40,
        gate_open: true,
    };
    assert_eq!(BotEvent::from_core(&level), None);
    assert_eq!(
        parse_command(r#"{"cmd":"set-noise-gate","db":-40}"#),
        Ok(BotCommand::SetNoiseGate { db: Some(-40) })
    );
    assert_eq!(
        parse_command(r#"{"cmd":"set_noise_gate","db":null}"#),
        Ok(BotCommand::SetNoiseGate { db: None })
    );
    assert_eq!(
        BotCommand::SetNoiseGate { db: Some(-40) }.to_core(),
        Some(Command::SetNoiseGate(Some(-40)))
    );
}

#[test]
fn participant_volume_command_parses_and_translates() {
    assert_eq!(
        parse_command(r#"{"cmd":"set-participant-volume","identity":"alice","percent":70}"#),
        Ok(BotCommand::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 70,
        })
    );
    assert_eq!(
        parse_command(r#"{"cmd":"set_participant_volume","identity":"alice","percent":150}"#),
        Ok(BotCommand::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 150,
        })
    );
    assert_eq!(
        BotCommand::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 70,
        }
        .to_core(),
        Some(Command::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 70,
        })
    );
}

#[test]
fn admin_commands_errors_and_events_round_trip() {
    assert_eq!(
        parse_command(r#"{"cmd":"admin-login","password":"pw"}"#),
        Ok(BotCommand::AdminLogin {
            password: "pw".to_owned()
        })
    );
    assert_eq!(
        BotCommand::AdminMoveParticipant {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
            to_room: "jogos".to_owned(),
        }
        .to_core(),
        Some(Command::Admin(AdminAction::MoveParticipant {
            room: "geral".to_owned(),
            identity: "ana".to_owned(),
            to_room: "jogos".to_owned(),
        }))
    );
    assert_eq!(
        error_kind(&SessionError::AdminInvalidPassword),
        "admin_invalid_password"
    );
    assert_eq!(error_kind(&SessionError::AdminDisabled), "admin_disabled");
    assert_eq!(
        BotEvent::from_core(&Event::RemovedFromRoom {
            reason: RemovalReason::RoomDeleted,
        }),
        Some(BotEvent::RemovedFromRoom {
            reason: "room_deleted".to_owned(),
        })
    );
    assert_eq!(
        BotEvent::from_core(&Event::MovedByAdmin {
            to_room: "afk".to_owned(),
        }),
        Some(BotEvent::MovedByAdmin {
            to_room: "afk".to_owned(),
        })
    );
}
