//! Utilitários compartilhados entre os testes unitários do módulo de estado.

use std::collections::BTreeMap;

use super::room::RoomState;
use super::{AppState, Screen, UserAction};
use crate::settings::{NoiseMode, Settings, VoiceMode};
use crate::ui_model::{
    AudioDeviceView, ChatMessageView, FileTransferView, ParticipantView, TransferStateView, UiEvent,
};

pub(super) fn valid_settings() -> Settings {
    Settings {
        client_id: "0123456789abcdef0123456789abcdef".to_owned(),
        display_name: "Pedro".to_owned(),
        token_server_url: "http://localhost:8080".to_owned(),
        last_room: "geral".to_owned(),
        muted_on_join: false,
        input_device: None,
        output_device: None,
        noise_suppression: NoiseMode::default(),
        voice_mode: VoiceMode::default(),
        stream_volume: None,
        participant_volumes: BTreeMap::new(),
        noise_gate_db: None,
        mute_hotkey: None,
        deafen_hotkey: None,
        ptt_hotkey: None,
        close_to_tray: true,
        room_sounds: true,
        dismissed_update_version: None,
    }
}

pub(super) fn mic(id: &str) -> AudioDeviceView {
    AudioDeviceView {
        id: id.to_owned(),
        name: format!("Mic {id}"),
    }
}

pub(super) fn devices_event(inputs: &[&str], outputs: &[&str]) -> UiEvent {
    UiEvent::AudioDevicesUpdated {
        inputs: inputs.iter().map(|id| mic(id)).collect(),
        outputs: outputs.iter().map(|id| mic(id)).collect(),
        selected_input: None,
        selected_output: None,
    }
}

pub(super) fn state_with_code() -> AppState {
    let mut state = AppState::new(&valid_settings(), None);
    state.form.invite_code = "segredo".to_owned();
    state
}

pub(super) fn person(identity: &str) -> ParticipantView {
    ParticipantView {
        identity: identity.to_owned(),
        name: identity.to_uppercase(),
        muted: false,
        speaking: false,
    }
}

pub(super) fn message(id: usize) -> ChatMessageView {
    ChatMessageView {
        id: id.to_string(),
        sender_id: "ana".to_owned(),
        sender: "ANA".to_owned(),
        sent_at_unix_ms: 0,
        text: format!("msg {id}"),
        saved: false,
    }
}

pub(super) fn saved_message(
    id: &str,
    sender_id: &str,
    sender: &str,
    text: &str,
) -> ChatMessageView {
    ChatMessageView {
        id: id.to_owned(),
        sender_id: sender_id.to_owned(),
        sender: sender.to_owned(),
        sent_at_unix_ms: 1000,
        text: text.to_owned(),
        saved: true,
    }
}

pub(super) fn chat_msg(id: &str, sender_id: &str, saved: bool) -> ChatMessageView {
    ChatMessageView {
        saved,
        ..saved_message(id, sender_id, &sender_id.to_uppercase(), id)
    }
}

pub(super) fn transfer(id: &str, outgoing: bool, state: TransferStateView) -> FileTransferView {
    FileTransferView {
        id: id.to_owned(),
        sender: "ana".to_owned(),
        name: "foto.png".to_owned(),
        size: 100,
        is_image: true,
        outgoing,
        sent_at_unix_ms: 0,
        state,
    }
}

pub(super) fn room_mut(state: &mut AppState) -> &mut RoomState {
    match &mut state.screen {
        Screen::InRoom(room) => room,
        _ => panic!("not in a room"),
    }
}

pub(super) fn in_room(state: &mut AppState) {
    if matches!(state.screen, Screen::Onboarding) {
        state.handle_action(UserAction::SubmitOnboarding);
    }
    state.handle_action(UserAction::JoinRoom("geral".to_owned()));
    state.apply_event(UiEvent::Joined {
        room: "geral".to_owned(),
        local_identity: "pedro".to_owned(),
        participants: vec![person("ana")],
    });
}

pub(super) fn room(state: &AppState) -> &RoomState {
    match &state.screen {
        Screen::InRoom(room) => room,
        other => panic!("expected InRoom, got {other:?}"),
    }
}
