//! Estado da sala conectada, participantes e transições entre lobby e sala.

use std::time::{Duration, Instant};

use rooms_core::Cue;
use rooms_protocol::RoomSummary;

use super::admin::AdminDialog;
use super::audio::{
    FULL_VOLUME, LobbyVoiceState, saved_volume_commands, toggle_deafen_fields, toggle_mute_fields,
};
use super::chat::ChatState;
use super::share::ShareState;
use super::{AppState, Screen};
use crate::settings::{NoiseMode, VoiceMode};
use crate::ui_model::{ParticipantView, UiCommand, UiEvent};

pub const ROOMS_REFRESH_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Reconnecting,
}

impl ConnectionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Conectando…",
            Self::Connected => "Conectado",
            Self::Reconnecting => "Reconectando…",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomState {
    pub room: String,
    pub local_identity: String,
    pub participants: Vec<ParticipantView>,
    pub status: ConnectionStatus,
    pub muted: bool,
    pub deafened: bool,
    muted_before_deafen: bool,
    pub ptt_held: bool,
    pub ptt_release_at: Option<Instant>,
    pub mic_sent: bool,
    pub chat: ChatState,
    pub share: ShareState,
}

impl RoomState {
    pub(super) fn new(
        room: String,
        local_identity: String,
        participants: Vec<ParticipantView>,
        voice: LobbyVoiceState,
        mode: VoiceMode,
    ) -> Self {
        let mic_sent = voice.effective_muted(mode);
        Self {
            room,
            local_identity,
            participants,
            status: ConnectionStatus::Connected,
            muted: voice.muted,
            deafened: voice.deafened,
            muted_before_deafen: voice.muted_before_deafen,
            ptt_held: false,
            ptt_release_at: None,
            mic_sent,
            chat: ChatState::new(),
            share: ShareState::default(),
        }
    }

    pub(super) fn voice_state(&self) -> LobbyVoiceState {
        LobbyVoiceState {
            muted: self.muted,
            deafened: self.deafened,
            muted_before_deafen: self.muted_before_deafen,
        }
    }

    fn ptt_active(&self) -> bool {
        self.ptt_held || self.ptt_release_at.is_some()
    }

    pub(super) fn effective_muted(&self, mode: VoiceMode) -> bool {
        self.muted || (mode == VoiceMode::PushToTalk && !self.ptt_active())
    }

    pub(super) fn sync_mic(&mut self, mode: VoiceMode) -> Option<UiCommand> {
        let effective = self.effective_muted(mode);
        (effective != self.mic_sent).then(|| {
            self.mic_sent = effective;
            UiCommand::SetMuted(effective)
        })
    }

    fn upsert_participant(&mut self, participant: ParticipantView) {
        match self
            .participants
            .iter_mut()
            .find(|p| p.identity == participant.identity)
        {
            Some(existing) => *existing = participant,
            None => self.participants.push(participant),
        }
    }

    pub(super) fn toggle_mute(&mut self, mode: VoiceMode) -> Vec<UiCommand> {
        let ptt_active = self.ptt_active();
        toggle_mute_fields(
            &mut self.muted,
            self.deafened,
            &mut self.mic_sent,
            ptt_active,
            mode,
        )
    }

    pub(super) fn toggle_deafen(&mut self, mode: VoiceMode) -> Vec<UiCommand> {
        let ptt_active = self.ptt_active();
        toggle_deafen_fields(
            &mut self.muted,
            &mut self.deafened,
            &mut self.muted_before_deafen,
            &mut self.mic_sent,
            ptt_active,
            mode,
        )
    }

    fn reset_for_room(&mut self, room: String) -> Option<std::path::PathBuf> {
        self.room = room;
        self.participants.clear();
        self.share = ShareState::default();
        self.chat.clear()
    }
}

pub fn room_display_title<'a>(rooms: &'a [RoomSummary], room_id: &'a str) -> &'a str {
    rooms
        .iter()
        .find(|r| r.name == room_id)
        .map_or(room_id, RoomSummary::display_title)
}

impl AppState {
    pub fn mark_refreshed(&mut self, now: Instant) {
        self.last_refresh = Some(now);
    }

    pub fn poll_refresh(&mut self, now: Instant) -> Option<UiCommand> {
        if !matches!(self.screen, Screen::Lobby | Screen::InRoom(_)) {
            return None;
        }
        let due = self
            .last_refresh
            .is_none_or(|last| now.saturating_duration_since(last) >= ROOMS_REFRESH_INTERVAL);
        if due {
            self.last_refresh = Some(now);
            Some(UiCommand::RefreshRooms)
        } else {
            None
        }
    }

    pub(super) fn with_room(
        &mut self,
        f: impl FnOnce(&mut RoomState) -> Vec<UiCommand>,
    ) -> Vec<UiCommand> {
        match &mut self.screen {
            Screen::InRoom(state) => f(state),
            _ => Vec::new(),
        }
    }

    pub(super) fn join_room(&mut self, room: String) -> Vec<UiCommand> {
        if !matches!(self.screen, Screen::Lobby) || room.trim().is_empty() {
            return Vec::new();
        }
        let mut commands = Vec::new();
        if !self.audio_configured {
            if let Some(id) = self.audio.input.clone() {
                commands.push(UiCommand::SetInputDevice(Some(id)));
            }
            if let Some(id) = self.audio.output.clone() {
                commands.push(UiCommand::SetOutputDevice(Some(id)));
            }
            if self.audio.stream_volume != FULL_VOLUME {
                commands.push(UiCommand::SetStreamVolume(self.audio.stream_volume));
            }
            if self.audio.noise != NoiseMode::default() {
                commands.push(UiCommand::SetNoiseSuppression(self.audio.noise));
            }
            if let Some(threshold) = self.audio.noise_gate {
                commands.push(UiCommand::SetNoiseGate(Some(threshold)));
            }
            commands.extend(saved_volume_commands(&self.audio.participant_volumes));
            self.audio_configured = true;
        }
        if self.lobby_voice.deafened != self.session_deafened {
            commands.push(UiCommand::SetDeafened(self.lobby_voice.deafened));
            self.session_deafened = self.lobby_voice.deafened;
        }
        let desired_mic = self.lobby_voice.effective_muted(self.audio.voice_mode);
        if desired_mic != self.session_mic_sent {
            commands.push(UiCommand::SetMuted(desired_mic));
            self.session_mic_sent = desired_mic;
        }
        commands.push(UiCommand::Join { room: room.clone() });
        self.lobby_error = None;
        self.lobby_notice = None;
        self.pending_move_notice = None;
        self.audio.open = false;
        self.screen = Screen::Connecting { room };
        commands
    }

    pub(super) fn cancel_connect(&mut self) -> Vec<UiCommand> {
        self.update.on_left_room();
        if matches!(self.screen, Screen::Connecting { .. }) {
            self.pending_move_notice = None;
            self.screen = Screen::Lobby;
            vec![UiCommand::Leave]
        } else {
            Vec::new()
        }
    }

    pub(super) fn leave(&mut self) -> Vec<UiCommand> {
        self.update.on_left_room();
        self.picker.close();
        self.audio.mic_level = None;
        self.pending_move_notice = None;
        if matches!(
            &self.admin.dialog,
            Some(AdminDialog::KickParticipant { .. })
        ) {
            self.admin.dialog = None;
        }
        let leave_cue = self.self_left_cue();
        if let Screen::InRoom(room) = &self.screen {
            self.lobby_voice = room.voice_state();
            self.session_mic_sent = room.mic_sent;
            self.session_deafened = room.deafened;
            self.discard_pending_paste();
            self.screen = Screen::Lobby;
            leave_cue.into_iter().chain([UiCommand::Leave]).collect()
        } else {
            Vec::new()
        }
    }

    pub(super) fn switch_room(&mut self, room: String) -> Vec<UiCommand> {
        if !matches!(&self.screen, Screen::InRoom(state) if state.room != room) {
            return Vec::new();
        }
        self.pending_move_notice = None;
        if matches!(
            &self.admin.dialog,
            Some(AdminDialog::KickParticipant { .. })
        ) {
            self.admin.dialog = None;
        }
        let leave_cue = self.self_left_cue();
        let discarded = if let Screen::InRoom(state) = &mut self.screen {
            state.status = ConnectionStatus::Connecting;
            state.reset_for_room(room.clone())
        } else {
            None
        };
        if let Some(path) = discarded {
            self.pending_paste_deletes.push(path);
        }
        leave_cue
            .into_iter()
            .chain([UiCommand::Join { room }])
            .collect()
    }

    pub(super) fn on_joined(
        &mut self,
        room: String,
        local_identity: String,
        participants: Vec<ParticipantView>,
    ) {
        match &mut self.screen {
            Screen::Connecting { .. } => {
                let mut state = RoomState::new(
                    room,
                    local_identity,
                    participants,
                    self.lobby_voice,
                    self.audio.voice_mode,
                );
                if let Some(notice) = self.pending_move_notice.take() {
                    state.chat.set_notice(notice);
                }
                self.session_mic_sent = state.mic_sent;
                self.session_deafened = state.deafened;
                self.lobby_error = None;
                self.lobby_notice = None;
                self.screen = Screen::InRoom(Box::new(state));
                self.last_refresh = None;
                self.pending_commands.extend(self.room_cue(Cue::SelfJoined));
            }
            Screen::InRoom(state) => {
                let is_new_room =
                    state.room != room || state.status == ConnectionStatus::Connecting;
                let discarded = if state.room != room {
                    state.reset_for_room(room)
                } else {
                    None
                };
                state.local_identity = local_identity;
                state.participants = participants;
                state.status = ConnectionStatus::Connected;
                state.share = ShareState::default();
                if let Some(notice) = self.pending_move_notice.take() {
                    state.chat.set_notice(notice);
                }
                if let Some(path) = discarded {
                    self.pending_paste_deletes.push(path);
                }
                if is_new_room {
                    self.pending_commands.extend(self.room_cue(Cue::SelfJoined));
                }
            }
            Screen::Onboarding | Screen::Lobby => {}
        }
    }

    pub(super) fn leave_to_lobby(&mut self) {
        self.update.on_left_room();
        let leave_cue = self.self_left_cue();
        self.picker.close();
        self.audio.mic_level = None;
        if matches!(
            &self.admin.dialog,
            Some(AdminDialog::KickParticipant { .. })
        ) {
            self.admin.dialog = None;
        }
        match &self.screen {
            Screen::InRoom(room) => {
                self.lobby_voice = room.voice_state();
                self.session_mic_sent = room.mic_sent;
                self.session_deafened = room.deafened;
                self.discard_pending_paste();
                self.screen = Screen::Lobby;
                self.pending_commands.extend(leave_cue);
            }
            Screen::Connecting { .. } => {
                self.screen = Screen::Lobby;
            }
            Screen::Onboarding | Screen::Lobby => {}
        }
    }
}

pub(super) fn apply_room_event(state: &mut RoomState, event: UiEvent) -> Option<Cue> {
    match event {
        UiEvent::Connecting => {
            state.status = ConnectionStatus::Connecting;
            None
        }
        UiEvent::Reconnecting => {
            state.status = ConnectionStatus::Reconnecting;
            None
        }
        UiEvent::Reconnected => {
            state.status = ConnectionStatus::Connected;
            None
        }
        UiEvent::ParticipantJoined(p) => {
            let is_new_peer = state.status == ConnectionStatus::Connected
                && p.identity != state.local_identity
                && state
                    .participants
                    .iter()
                    .all(|existing| existing.identity != p.identity);
            state.upsert_participant(p);
            is_new_peer.then_some(Cue::PeerJoined)
        }
        UiEvent::ParticipantUpdated(p) => {
            state.upsert_participant(p);
            None
        }
        UiEvent::ParticipantLeft { identity } => {
            let was_peer = state.status == ConnectionStatus::Connected
                && identity != state.local_identity
                && state
                    .participants
                    .iter()
                    .any(|existing| existing.identity == identity);
            state.participants.retain(|p| p.identity != identity);
            state.share.apply(UiEvent::ParticipantLeft { identity });
            was_peer.then_some(Cue::PeerLeft)
        }
        UiEvent::ChatReceived(message) => {
            state.chat.push_message(message, &state.local_identity);
            None
        }
        UiEvent::ChatHistory {
            messages,
            has_more,
            before: _,
        } => {
            state.chat.apply_history(messages, has_more);
            None
        }
        UiEvent::ChatDeleted { id, sender } => {
            state.chat.delete_message(&id, &sender);
            None
        }
        UiEvent::ChatFailed { message } => {
            state.chat.handle_failed(message);
            None
        }
        UiEvent::FileTransfer(view) => {
            state.chat.apply_transfer(view);
            None
        }
        share @ (UiEvent::ScreenShareStarted { .. }
        | UiEvent::ScreenShareStopped
        | UiEvent::ShareFailed { .. }
        | UiEvent::RemoteScreenShareStarted { .. }
        | UiEvent::RemoteScreenShareStopped { .. }
        | UiEvent::WatchingStarted { .. }
        | UiEvent::WatchingStopped) => {
            state.share.apply(share);
            None
        }
        UiEvent::Joined { .. }
        | UiEvent::Left
        | UiEvent::Failed { .. }
        | UiEvent::InviteRejected
        | UiEvent::RoomsFailed { .. }
        | UiEvent::RoomsUpdated(_)
        | UiEvent::CaptureTargetsUpdated(_)
        | UiEvent::CaptureThumbnail { .. }
        | UiEvent::MicLevel { .. }
        | UiEvent::AudioDevicesUpdated { .. }
        | UiEvent::AdminChanged { .. }
        | UiEvent::AdminRoomCreated(_)
        | UiEvent::AdminRoomUpdated(_)
        | UiEvent::AdminRoomDeleted { .. }
        | UiEvent::AdminInviteRotated
        | UiEvent::AdminStoppedScreenShare
        | UiEvent::MovedByAdmin { .. }
        | UiEvent::LocalAvatarChanged { .. }
        | UiEvent::AvatarCached { .. }
        | UiEvent::AvatarFailed { .. }
        | UiEvent::RemovedFromRoom { .. }
        | UiEvent::AdminFailed { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{
        in_room, message, person, room, state_with_code, valid_settings,
    };

    #[test]
    fn join_flow_goes_onboarding_lobby_connecting_in_room() {
        let mut state = state_with_code();
        assert_eq!(state.screen, Screen::Onboarding);
        assert_eq!(
            state.handle_action(UserAction::SubmitOnboarding),
            vec![UiCommand::RefreshRooms]
        );
        assert_eq!(state.screen, Screen::Lobby);
        assert_eq!(state.take_pending_save(), Some(valid_settings()));
        assert!(state.take_pending_save().is_none());

        let commands = state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert_eq!(
            commands,
            vec![UiCommand::Join {
                room: "geral".to_owned()
            }]
        );
        assert_eq!(
            state.screen,
            Screen::Connecting {
                room: "geral".to_owned()
            }
        );

        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("ana")],
        });
        let room = room(&state);
        assert_eq!(room.room, "geral");
        assert_eq!(room.local_identity, "pedro");
        assert_eq!(room.participants.len(), 1);
        assert_eq!(room.status, ConnectionStatus::Connected);
    }

    #[test]
    fn muted_on_join_sends_mute_first_and_starts_muted() {
        let mut state = state_with_code();
        state.muted_on_join = true;
        state.handle_action(UserAction::SubmitOnboarding);
        let commands = state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert_eq!(
            commands,
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
        assert!(room(&state).muted);
    }

    #[test]
    fn failure_returns_to_lobby_with_error_and_keeps_fields() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        state.apply_event(UiEvent::Failed {
            message: "sem rede".to_owned(),
        });
        assert_eq!(state.screen, Screen::Lobby);
        assert_eq!(state.lobby_error.as_deref(), Some("sem rede"));
        assert_eq!(state.form.display_name, "Pedro");
        assert_eq!(state.credentials().invite_code, "segredo");
    }

    #[test]
    fn stale_events_are_ignored_on_onboarding() {
        let mut state = state_with_code();
        state.apply_event(UiEvent::Failed {
            message: "x".to_owned(),
        });
        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        state.apply_event(UiEvent::RoomsUpdated(vec![RoomSummary {
            name: "geral".to_owned(),
            title: String::new(),
            participants: vec![],
        }]));
        assert_eq!(state.screen, Screen::Onboarding);
        assert!(state.rooms.is_empty());
        assert!(state.take_credential_ops().is_empty());
    }

    #[test]
    fn cancel_connect_returns_to_lobby_and_leaves() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert_eq!(
            state.handle_action(UserAction::CancelConnect),
            vec![UiCommand::Leave]
        );
        assert_eq!(state.screen, Screen::Lobby);
    }

    #[test]
    fn leave_and_left_event_return_to_lobby() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.take_pending_commands();
        assert_eq!(
            state.handle_action(UserAction::Leave),
            vec![UiCommand::PlayCue(Cue::SelfLeft), UiCommand::Leave]
        );
        assert_eq!(state.screen, Screen::Lobby);

        in_room(&mut state);
        state.take_pending_commands();
        state.apply_event(UiEvent::Left);
        assert_eq!(state.screen, Screen::Lobby);
        assert_eq!(
            state.take_pending_commands(),
            vec![UiCommand::PlayCue(Cue::SelfLeft)]
        );
    }

    #[test]
    fn participants_join_leave_and_update() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ParticipantJoined(person("bruno")));
        assert_eq!(room(&state).participants.len(), 2);

        let mut updated = person("ana");
        updated.muted = true;
        updated.speaking = true;
        state.apply_event(UiEvent::ParticipantUpdated(updated.clone()));
        assert_eq!(room(&state).participants[0], updated);
        assert_eq!(room(&state).participants.len(), 2);

        state.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert_eq!(room(&state).participants, vec![person("bruno")]);
    }

    #[test]
    fn reconnecting_updates_status() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::Reconnecting);
        assert_eq!(room(&state).status, ConnectionStatus::Reconnecting);
        state.apply_event(UiEvent::Reconnected);
        assert_eq!(room(&state).status, ConnectionStatus::Connected);
    }

    #[test]
    fn switching_room_clears_chat_and_emits_join() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(message(1)));
        state.take_pending_save();
        let commands = state.handle_action(UserAction::SwitchRoom("jogos".to_owned()));
        assert_eq!(
            commands,
            vec![
                UiCommand::PlayCue(Cue::SelfLeft),
                UiCommand::Join {
                    room: "jogos".to_owned()
                }
            ]
        );
        let current = room(&state);
        assert_eq!(current.room, "jogos");
        assert!(current.chat.messages.is_empty());
        assert!(current.participants.is_empty());
        assert_eq!(current.status, ConnectionStatus::Connecting);
        assert!(state.take_pending_save().is_none());

        assert!(
            state
                .handle_action(UserAction::SwitchRoom("jogos".to_owned()))
                .is_empty()
        );
    }

    #[test]
    fn joined_with_other_room_clears_chat() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.apply_event(UiEvent::ChatReceived(message(1)));
        state.apply_event(UiEvent::Joined {
            room: "musica".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![],
        });
        assert_eq!(room(&state).room, "musica");
        assert!(room(&state).chat.messages.is_empty());
    }

    #[test]
    fn rooms_updated_replaces_the_list() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        let summary = |name: &str| RoomSummary {
            name: name.to_owned(),
            title: String::new(),
            participants: vec![],
        };
        state.apply_event(UiEvent::RoomsUpdated(vec![summary("a"), summary("b")]));
        assert_eq!(state.rooms.len(), 2);
        state.apply_event(UiEvent::RoomsUpdated(vec![summary("c")]));
        assert_eq!(state.rooms, vec![summary("c")]);
    }

    #[test]
    fn rooms_updated_sorts_occupants_by_name() {
        let mut state = state_with_code();
        state.handle_action(UserAction::SubmitOnboarding);
        let room = |people: &[&str]| RoomSummary {
            name: "geral".to_owned(),
            title: String::new(),
            participants: people.iter().map(|p| (*p).to_owned()).collect(),
        };
        state.apply_event(UiEvent::RoomsUpdated(vec![room(&["Pedro", "bel", "KNC"])]));
        assert_eq!(state.rooms, vec![room(&["bel", "KNC", "Pedro"])]);
        state.apply_event(UiEvent::RoomsUpdated(vec![room(&["KNC", "Pedro", "bel"])]));
        assert_eq!(state.rooms, vec![room(&["bel", "KNC", "Pedro"])]);
    }

    #[test]
    fn refresh_runs_in_lobby_and_in_room_only() {
        let mut state = state_with_code();
        let start = Instant::now();
        assert!(state.poll_refresh(start).is_none());

        state.handle_action(UserAction::SubmitOnboarding);
        assert_eq!(state.poll_refresh(start), Some(UiCommand::RefreshRooms));
        assert!(state.poll_refresh(start + Duration::from_secs(2)).is_none());
        assert_eq!(
            state.poll_refresh(start + ROOMS_REFRESH_INTERVAL),
            Some(UiCommand::RefreshRooms)
        );

        state.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(
            state
                .poll_refresh(start + ROOMS_REFRESH_INTERVAL * 2)
                .is_none()
        );

        state.apply_event(UiEvent::Joined {
            room: "geral".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("ana")],
        });
        assert_eq!(state.poll_refresh(start), Some(UiCommand::RefreshRooms));
        assert!(state.poll_refresh(start).is_none());
        assert!(state.poll_refresh(start + Duration::from_secs(2)).is_none());
        assert_eq!(
            state.poll_refresh(start + ROOMS_REFRESH_INTERVAL),
            Some(UiCommand::RefreshRooms)
        );
    }

    #[test]
    fn room_display_title_uses_title_when_present_and_falls_back_to_id() {
        let rooms = vec![
            RoomSummary {
                name: "sala-1".to_owned(),
                title: "Bate-papo".to_owned(),
                participants: Vec::new(),
            },
            RoomSummary {
                name: "geral".to_owned(),
                title: String::new(),
                participants: Vec::new(),
            },
        ];

        assert_eq!(room_display_title(&rooms, "sala-1"), "Bate-papo");
        assert_eq!(room_display_title(&rooms, "geral"), "geral");
        assert_eq!(room_display_title(&rooms, "desconhecida"), "desconhecida");
    }
}
