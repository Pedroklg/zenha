//! Estado local de mudo, ensurdecimento e sons de eventos da sala.

use zenha_core::Cue;

use crate::platform::settings::VoiceMode;
use crate::state::{AppState, Screen};
use crate::ui_model::UiCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LobbyVoiceState {
    pub muted: bool,
    pub deafened: bool,
    pub(in crate::state) muted_before_deafen: bool,
}

impl LobbyVoiceState {
    pub(in crate::state) fn from_muted(muted: bool) -> Self {
        Self {
            muted,
            deafened: false,
            muted_before_deafen: muted,
        }
    }

    pub fn effective_muted(self, mode: VoiceMode) -> bool {
        self.muted || mode == VoiceMode::PushToTalk
    }
}

pub(in crate::state) fn toggle_mute_fields(
    muted: &mut bool,
    deafened: bool,
    mic_sent: &mut bool,
    ptt_active: bool,
    mode: VoiceMode,
) -> Vec<UiCommand> {
    if deafened {
        return Vec::new();
    }
    *muted = !*muted;
    let cue = if *muted { Cue::Muted } else { Cue::Unmuted };
    let effective = *muted || (mode == VoiceMode::PushToTalk && !ptt_active);
    let mut commands = Vec::new();
    if effective != *mic_sent {
        *mic_sent = effective;
        commands.push(UiCommand::SetMuted(effective));
    }
    commands.push(UiCommand::PlayCue(cue));
    commands
}

pub(in crate::state) fn toggle_deafen_fields(
    muted: &mut bool,
    deafened: &mut bool,
    muted_before_deafen: &mut bool,
    mic_sent: &mut bool,
    ptt_active: bool,
    mode: VoiceMode,
) -> Vec<UiCommand> {
    if *deafened {
        *deafened = false;
        *muted = *muted_before_deafen;
        let effective = *muted || (mode == VoiceMode::PushToTalk && !ptt_active);
        *mic_sent = effective;
        vec![
            UiCommand::SetDeafened(false),
            UiCommand::SetMuted(effective),
            UiCommand::PlayCue(Cue::Undeafened),
        ]
    } else {
        *muted_before_deafen = *muted;
        *deafened = true;
        *muted = true;
        *mic_sent = true;
        vec![
            UiCommand::SetDeafened(true),
            UiCommand::SetMuted(true),
            UiCommand::PlayCue(Cue::Deafened),
        ]
    }
}

impl AppState {
    fn is_deafened(&self) -> bool {
        match &self.screen {
            Screen::InRoom(room) => room.deafened,
            Screen::Onboarding | Screen::Lobby | Screen::Connecting { .. } => {
                self.lobby_voice.deafened
            }
        }
    }

    pub(in crate::state) fn room_cue(&self, cue: Cue) -> Option<UiCommand> {
        (self.room_sounds && !self.is_deafened()).then_some(UiCommand::PlayCue(cue))
    }

    pub(in crate::state) fn self_left_cue(&self) -> Option<UiCommand> {
        matches!(self.screen, Screen::InRoom(_))
            .then(|| self.room_cue(Cue::SelfLeft))
            .flatten()
    }

    pub(in crate::state) fn toggle_mute(&mut self) -> Vec<UiCommand> {
        let mode = self.audio.voice_mode;
        match &mut self.screen {
            Screen::InRoom(room) => {
                let commands = room.toggle_mute(mode);
                self.session_mic_sent = room.mic_sent;
                commands
            }
            Screen::Lobby => toggle_mute_fields(
                &mut self.lobby_voice.muted,
                self.lobby_voice.deafened,
                &mut self.session_mic_sent,
                false,
                mode,
            ),
            Screen::Onboarding | Screen::Connecting { .. } => Vec::new(),
        }
    }

    pub(in crate::state) fn toggle_deafen(&mut self) -> Vec<UiCommand> {
        let mode = self.audio.voice_mode;
        match &mut self.screen {
            Screen::InRoom(room) => {
                let commands = room.toggle_deafen(mode);
                self.session_mic_sent = room.mic_sent;
                self.session_deafened = room.deafened;
                commands
            }
            Screen::Lobby => {
                let mut mic_sent = self.session_mic_sent;
                let mut deafened = self.lobby_voice.deafened;
                let commands = toggle_deafen_fields(
                    &mut self.lobby_voice.muted,
                    &mut deafened,
                    &mut self.lobby_voice.muted_before_deafen,
                    &mut mic_sent,
                    false,
                    mode,
                );
                self.lobby_voice.deafened = deafened;
                self.session_mic_sent = mic_sent;
                self.session_deafened = deafened;
                commands
            }
            Screen::Onboarding | Screen::Connecting { .. } => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::hotkeys::HotkeyAction;
    use crate::state::UserAction;
    use crate::state::test_support::{in_room, person, room, state_with_code, valid_settings};
    use crate::ui_model::UiEvent;

    #[test]
    fn mute_toggles_and_emits_command() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert!(room(&state).muted);
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(false), UiCommand::PlayCue(Cue::Unmuted)]
        );
    }

    #[test]
    fn deafen_mutes_and_undeafen_restores_previous_mute_state() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened)
            ]
        );
        assert!(room(&state).deafened && room(&state).muted);
        assert!(state.handle_action(UserAction::ToggleMute).is_empty());
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(false),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
        assert!(!room(&state).muted && !room(&state).deafened);

        state.handle_action(UserAction::ToggleMute);
        state.handle_action(UserAction::ToggleDeafen);
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
        assert!(room(&state).muted);
    }

    #[test]
    fn mute_and_deafen_cues_cover_all_transitions_and_skip_ignored_mute_and_join() {
        let mut state = state_with_code();
        state.muted_on_join = true;
        let onboarding_cmds = state.handle_action(UserAction::SubmitOnboarding);
        assert!(
            !onboarding_cmds
                .iter()
                .any(|c| matches!(c, UiCommand::PlayCue(_)))
        );
        let join_cmds = state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(!join_cmds.iter().any(|c| matches!(c, UiCommand::PlayCue(_))));
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("ana")],
        });
        assert!(room(&state).muted);

        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(false), UiCommand::PlayCue(Cue::Unmuted)]
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened)
            ]
        );
        assert!(state.handle_action(UserAction::ToggleMute).is_empty());
        assert!(
            state
                .handle_action(UserAction::HotkeyPressed(HotkeyAction::ToggleMute))
                .is_empty()
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
    }

    #[test]
    fn lobby_voice_state_carries_across_rooms_and_back_on_leave() {
        let mut state = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert!(state.lobby_voice.muted);

        assert_eq!(
            state.handle_action(UserAction::JoinRoom("geral".to_owned())),
            vec![UiCommand::Join {
                room: "geral".to_owned()
            }]
        );
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        assert!(room(&state).muted);

        state.handle_action(UserAction::ToggleDeafen);
        state.handle_action(UserAction::Leave);
        assert_eq!(state.screen, Screen::Lobby);
        assert!(state.lobby_voice.muted);
        assert!(state.lobby_voice.deafened);

        state.handle_action(UserAction::JoinRoom("jogos".to_owned()));
        state.apply_event(UiEvent::Joined {
            room: "jogos".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        assert!(room(&state).deafened);
        assert!(room(&state).muted);
    }

    #[test]
    fn muted_on_join_setting_updates_and_saves() {
        let mut state = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        assert!(!state.muted_on_join);
        state.handle_action(UserAction::SetMutedOnJoin(true));
        assert!(state.muted_on_join);
        assert!(state.lobby_voice.muted);
        assert!(state.take_pending_save().unwrap().muted_on_join);
        assert_eq!(
            state.handle_action(UserAction::JoinRoom("geral".to_owned())),
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
    }

    #[test]
    fn room_sounds_play_for_join_switch_leave_and_peer_changes_not_on_reconnect() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(state.take_pending_commands().is_empty());

        // Entering the room with peers already present only plays SelfJoined (no PeerJoined).
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("ana")],
        });
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::SelfJoined)]
        );

        // Another participant joining/leaving while connected emits PeerJoined / PeerLeft.
        state.apply_event(UiEvent::ParticipantJoined(person("bruno")));
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::PeerJoined)]
        );
        // Joining event for local user or duplicate update must not emit PeerJoined.
        state.apply_event(UiEvent::ParticipantJoined(person("pedro")));
        state.apply_event(UiEvent::ParticipantUpdated(person("bruno")));
        assert!(state.take_pending_commands().is_empty());

        state.apply_event(UiEvent::ParticipantLeft {
            identity: "bruno".to_owned(),
        });
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::PeerLeft)]
        );

        // Reconnect and subsequent ParticipantList/Joined must not emit any join/leave cue.
        state.apply_event(UiEvent::Reconnecting);
        state.apply_event(UiEvent::ParticipantJoined(person("carla")));
        state.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(state.take_pending_commands().is_empty());
        state.apply_event(UiEvent::Reconnected);
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("carla")],
        });
        assert!(state.take_pending_commands().is_empty());

        // Room switch emits SelfLeft immediately, then SelfJoined once the new room connects.
        let switch_cmds = state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        assert_eq!(
            switch_cmds,
            vec![
                UiCommand::PlayCue(Cue::SelfLeft),
                UiCommand::Join {
                    room: "jogos".to_owned(),
                },
            ]
        );
        state.apply_event(UiEvent::Joined {
            room: "jogos".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("ana")],
        });
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::SelfJoined)]
        );

        // CancelConnect from Lobby->Connecting must not emit SelfLeft.
        assert_eq!(
            state.handle_action(UserAction::Leave),
            vec![UiCommand::PlayCue(Cue::SelfLeft), UiCommand::Leave]
        );
        state.handle_action(UserAction::JoinRoom("musica".to_owned()));
        assert_eq!(
            state.handle_action(UserAction::CancelConnect),
            vec![UiCommand::Leave]
        );
    }

    #[test]
    fn room_sounds_are_suppressed_when_deafened_or_disabled_but_mute_cues_remain() {
        // Suppressed when deafened.
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::ToggleDeafen);
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro"), person("ana")],
        });
        state.apply_event(UiEvent::ParticipantJoined(person("bruno")));
        state.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(state.take_pending_commands().is_empty());
        assert_eq!(
            state.handle_action(UserAction::SwitchRoom("jogos".to_owned())),
            vec![UiCommand::Join {
                room: "jogos".to_owned(),
            }]
        );
        assert_eq!(
            state.handle_action(UserAction::Leave),
            vec![UiCommand::Leave]
        );
        // Un-deafening still plays Undeafened cue.
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(false),
                UiCommand::PlayCue(Cue::Undeafened),
            ]
        );

        // Suppressed when room_sounds setting is disabled, and setting persists.
        assert!(
            state
                .handle_action(UserAction::SetRoomSounds(false))
                .is_empty()
        );
        assert!(!state.room_sounds);
        let saved = state.take_pending_save().unwrap();
        assert!(!saved.room_sounds);

        let mut reloaded = AppState::new(&saved, Some("segredo".to_owned()));
        assert!(!reloaded.room_sounds);
        reloaded.handle_action(UserAction::JoinRoom("geral".to_owned()));
        reloaded.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("pedro")],
        });
        reloaded.apply_event(UiEvent::ParticipantJoined(person("ana")));
        reloaded.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(reloaded.take_pending_commands().is_empty());

        // Mute / unmute and deafen cues still work when room_sounds is false.
        assert_eq!(
            reloaded.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert_eq!(
            reloaded.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened),
            ]
        );
        assert_eq!(
            reloaded.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Undeafened),
            ]
        );
        assert_eq!(
            reloaded.handle_action(UserAction::Leave),
            vec![UiCommand::Leave]
        );
    }
}
