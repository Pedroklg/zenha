//! Testes das transições do `AppState`.

use crate::ui_model::UiEvent;

use super::test_support::{person, state_with_code, valid_settings};
use super::*;

#[test]
fn avatar_actions_events_and_credentials_follow_the_local_photo() {
    let mut state = AppState::new(&valid_settings(), None);
    assert!(state.credentials().avatar_version.is_none());
    assert!(state.handle_action(UserAction::RemoveAvatar).is_empty());
    let path = PathBuf::from("foto.png");
    assert_eq!(
        state.handle_action(UserAction::SetAvatar(path.clone())),
        vec![UiCommand::SetAvatar { path }]
    );

    state.apply_event(UiEvent::LocalAvatarChanged {
        version: Some("0123456789abcdef".to_owned()),
    });
    assert_eq!(state.avatar_version.as_deref(), Some("0123456789abcdef"));
    assert_eq!(
        state.take_pending_save().unwrap().avatar_version.as_deref(),
        Some("0123456789abcdef")
    );
    assert_eq!(
        state.credentials().avatar_version.as_deref(),
        Some("0123456789abcdef")
    );
    assert_eq!(
        state.handle_action(UserAction::RemoveAvatar),
        vec![UiCommand::ClearAvatar]
    );

    state.apply_event(UiEvent::AvatarFailed {
        message: "falhou".to_owned(),
    });
    assert_eq!(state.avatar_notice.as_deref(), Some("falhou"));
    state.apply_event(UiEvent::LocalAvatarChanged { version: None });
    assert!(state.avatar_notice.is_none());
    assert!(state.take_pending_save().unwrap().avatar_version.is_none());
}

#[test]
fn close_to_tray_loads_from_settings_and_saves_on_change() {
    let mut state = AppState::new(&valid_settings(), None);
    assert!(state.close_to_tray);
    assert!(
        state
            .handle_action(UserAction::SetCloseToTray(false))
            .is_empty()
    );
    assert!(!state.close_to_tray);
    let saved = state.take_pending_save().unwrap();
    assert!(!saved.close_to_tray);

    let restored = AppState::new(&saved, None);
    assert!(!restored.close_to_tray);
}

#[test]
fn tray_muted_reflects_room_presence_and_mute_state() {
    let mut state = state_with_code();
    assert_eq!(state.tray_muted(), None);

    state.handle_action(UserAction::SubmitOnboarding);
    assert_eq!(state.tray_muted(), None);

    state.handle_action(UserAction::JoinRoom("geral".to_owned()));
    assert_eq!(state.tray_muted(), None);

    state.apply_event(UiEvent::Joined {
        room: "geral".to_owned(),
        local_identity: "pedro".to_owned(),
        participants: vec![person("ana")],
    });
    assert_eq!(state.tray_muted(), Some(false));

    state.handle_action(UserAction::ToggleMute);
    assert_eq!(state.tray_muted(), Some(true));

    state.handle_action(UserAction::Leave);
    assert_eq!(state.tray_muted(), None);
}
