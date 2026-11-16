//! Controle de microfone por push-to-talk e seleção de modo de voz.

use std::time::{Duration, Instant};

use crate::hotkeys::binding_label;
use crate::settings::VoiceMode;
use crate::state::{AppState, Screen};
use crate::ui_model::UiCommand;

pub const PTT_RELEASE_DELAY: Duration = Duration::from_millis(200);

impl AppState {
    pub fn push_to_talk(&mut self, pressed: bool, now: Instant) -> Vec<UiCommand> {
        if self.audio.voice_mode != VoiceMode::PushToTalk {
            return Vec::new();
        }
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        let mode = self.audio.voice_mode;
        let commands: Vec<UiCommand> = if pressed {
            room.ptt_held = true;
            room.ptt_release_at = None;
            room.sync_mic(mode).into_iter().collect()
        } else {
            if room.ptt_held {
                room.ptt_held = false;
                room.ptt_release_at = Some(now + PTT_RELEASE_DELAY);
            }
            Vec::new()
        };
        self.session_mic_sent = room.mic_sent;
        commands
    }

    pub fn poll_push_to_talk(&mut self, now: Instant) -> (Vec<UiCommand>, Option<Duration>) {
        let Screen::InRoom(room) = &mut self.screen else {
            return (Vec::new(), None);
        };
        let Some(deadline) = room.ptt_release_at else {
            return (Vec::new(), None);
        };
        if now >= deadline {
            room.ptt_release_at = None;
            let commands = room.sync_mic(self.audio.voice_mode).into_iter().collect();
            self.session_mic_sent = room.mic_sent;
            (commands, None)
        } else {
            (Vec::new(), Some(deadline - now))
        }
    }

    pub fn push_to_talk_hint(&self) -> Option<String> {
        if self.audio.voice_mode != VoiceMode::PushToTalk {
            return None;
        }
        if let Screen::InRoom(room) = &self.screen
            && !room.effective_muted(VoiceMode::PushToTalk)
        {
            return Some("Falando (push-to-talk)".to_owned());
        }
        match self.hotkeys.ptt.as_deref() {
            Some(binding) => Some(format!("Segure {} para falar", binding_label(binding))),
            None => Some("Defina a tecla de push-to-talk nas Configurações".to_owned()),
        }
    }

    pub(in crate::state) fn select_voice_mode(&mut self, mode: VoiceMode) -> Vec<UiCommand> {
        if self.audio.voice_mode == mode {
            return Vec::new();
        }
        self.audio.voice_mode = mode;
        self.pending_save = Some(self.settings_snapshot());
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        room.ptt_held = false;
        room.ptt_release_at = None;
        let commands = room.sync_mic(mode).into_iter().collect();
        self.session_mic_sent = room.mic_sent;
        commands
    }
}

#[cfg(test)]
mod tests {
    use rooms_core::Cue;

    use super::*;
    use crate::settings::Settings;
    use crate::state::UserAction;
    use crate::state::test_support::{in_room, room, state_with_code, valid_settings};
    use crate::ui_model::UiEvent;

    fn ptt_state_in_room() -> AppState {
        let mut settings = valid_settings();
        settings.voice_mode = VoiceMode::PushToTalk;
        settings.ptt_hotkey = Some("control+Space".to_owned());
        let mut state = AppState::new(&settings, None);
        state.form.invite_code = "segredo".to_owned();
        in_room(&mut state);
        state.take_pending_save();
        state
    }

    #[test]
    fn join_from_lobby_in_push_to_talk_sends_single_mute_and_starts_unmuted_in_ui() {
        let mut settings = valid_settings();
        settings.voice_mode = VoiceMode::PushToTalk;
        let mut state = AppState::new(&settings, Some("segredo".to_owned()));
        assert_eq!(state.screen, Screen::Lobby);
        assert_eq!(
            state.handle_action(UserAction::JoinRoom("geral".to_owned())),
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        assert!(!room(&state).muted);
        assert!(room(&state).mic_sent);

        let mut both = AppState::new(
            &Settings {
                muted_on_join: true,
                ..settings
            },
            Some("segredo".to_owned()),
        );
        assert_eq!(
            both.handle_action(UserAction::JoinRoom("geral".to_owned())),
            vec![
                UiCommand::SetMuted(true),
                UiCommand::Join {
                    room: "geral".to_owned()
                }
            ]
        );
        both.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        assert!(room(&both).muted);
        assert!(room(&both).mic_sent);
    }

    #[test]
    fn push_to_talk_opens_on_press_and_closes_after_release_delay() {
        let mut state = ptt_state_in_room();
        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Segure Ctrl+Espaço para falar")
        );

        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert!(room(&state).ptt_held);
        assert!(!room(&state).mic_sent);
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Falando (push-to-talk)")
        );
        assert!(state.push_to_talk(true, t0).is_empty());

        assert!(state.push_to_talk(false, t0).is_empty());
        assert!(!room(&state).ptt_held);
        assert_eq!(room(&state).ptt_release_at, Some(t0 + PTT_RELEASE_DELAY));
        assert!(!room(&state).mic_sent);
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Falando (push-to-talk)")
        );

        let (cmds, wait) = state.poll_push_to_talk(t0 + Duration::from_millis(50));
        assert!(cmds.is_empty());
        assert_eq!(wait, Some(Duration::from_millis(150)));
        assert!(!room(&state).mic_sent);

        let (cmds, wait) = state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY);
        assert_eq!(cmds, vec![UiCommand::SetMuted(true)]);
        assert_eq!(wait, None);
        assert_eq!(room(&state).ptt_release_at, None);
        assert!(room(&state).mic_sent);
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Segure Ctrl+Espaço para falar")
        );
        assert_eq!(
            state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY),
            (Vec::new(), None)
        );
    }

    #[test]
    fn repressing_during_release_delay_cancels_timer_without_extra_command() {
        let mut state = ptt_state_in_room();
        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert!(state.push_to_talk(false, t0).is_empty());
        assert!(
            state
                .push_to_talk(true, t0 + Duration::from_millis(100))
                .is_empty()
        );
        assert!(room(&state).ptt_held);
        assert_eq!(room(&state).ptt_release_at, None);
        assert_eq!(
            state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY),
            (Vec::new(), None)
        );
        assert!(!room(&state).mic_sent);
    }

    #[test]
    fn manual_mute_overrides_push_to_talk() {
        let mut state = ptt_state_in_room();
        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(true), UiCommand::PlayCue(Cue::Muted)]
        );
        assert!(room(&state).muted);
        assert!(room(&state).mic_sent);
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Segure Ctrl+Espaço para falar")
        );

        assert!(state.push_to_talk(false, t0).is_empty());
        assert_eq!(
            state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY),
            (Vec::new(), None)
        );
        assert!(state.push_to_talk(true, t0 + PTT_RELEASE_DELAY).is_empty());
        assert!(room(&state).mic_sent);

        assert_eq!(
            state.handle_action(UserAction::ToggleMute),
            vec![UiCommand::SetMuted(false), UiCommand::PlayCue(Cue::Unmuted)]
        );
        assert!(!room(&state).muted);
        assert!(!room(&state).mic_sent);
    }

    #[test]
    fn deafen_during_push_to_talk_mutes_and_undeafen_respects_ptt_state() {
        let mut state = ptt_state_in_room();
        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(true),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Deafened)
            ]
        );
        assert!(room(&state).deafened);
        assert!(room(&state).mic_sent);

        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(false),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
        assert!(!room(&state).mic_sent);

        state.handle_action(UserAction::ToggleDeafen);
        state.push_to_talk(false, t0);
        state.poll_push_to_talk(t0 + PTT_RELEASE_DELAY);
        assert_eq!(
            state.handle_action(UserAction::ToggleDeafen),
            vec![
                UiCommand::SetDeafened(false),
                UiCommand::SetMuted(true),
                UiCommand::PlayCue(Cue::Undeafened)
            ]
        );
        assert!(!room(&state).muted);
        assert!(room(&state).mic_sent);
    }

    #[test]
    fn switching_voice_mode_in_room_resets_ptt_and_syncs_mic() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.take_pending_save();
        assert_eq!(state.push_to_talk_hint(), None);

        assert_eq!(
            state.handle_action(UserAction::SelectVoiceMode(VoiceMode::PushToTalk)),
            vec![UiCommand::SetMuted(true)]
        );
        assert_eq!(
            state.take_pending_save().unwrap().voice_mode,
            VoiceMode::PushToTalk
        );
        assert_eq!(
            state.push_to_talk_hint().as_deref(),
            Some("Defina a tecla de push-to-talk nas Configurações")
        );
        assert!(
            state
                .handle_action(UserAction::SelectVoiceMode(VoiceMode::PushToTalk))
                .is_empty()
        );

        let t0 = Instant::now();
        assert_eq!(
            state.push_to_talk(true, t0),
            vec![UiCommand::SetMuted(false)]
        );
        assert!(
            state
                .handle_action(UserAction::SelectVoiceMode(VoiceMode::VoiceActivity))
                .is_empty()
        );
        assert!(!room(&state).ptt_held);
        assert_eq!(room(&state).ptt_release_at, None);
        assert!(!room(&state).mic_sent);

        assert_eq!(
            state.handle_action(UserAction::SelectVoiceMode(VoiceMode::PushToTalk)),
            vec![UiCommand::SetMuted(true)]
        );
        assert_eq!(
            state.handle_action(UserAction::SelectVoiceMode(VoiceMode::VoiceActivity)),
            vec![UiCommand::SetMuted(false)]
        );
    }

    #[test]
    fn push_to_talk_outside_room_or_in_voice_activity_mode_does_nothing() {
        let t0 = Instant::now();
        let mut state = state_with_code();
        state.handle_action(UserAction::SelectVoiceMode(VoiceMode::PushToTalk));
        assert!(state.push_to_talk(true, t0).is_empty());
        assert!(state.push_to_talk(false, t0).is_empty());
        assert_eq!(state.poll_push_to_talk(t0), (Vec::new(), None));

        let mut va = state_with_code();
        in_room(&mut va);
        assert!(va.push_to_talk(true, t0).is_empty());
        assert!(va.push_to_talk(false, t0).is_empty());
        assert!(!room(&va).ptt_held);
        assert_eq!(va.poll_push_to_talk(t0), (Vec::new(), None));
    }
}
