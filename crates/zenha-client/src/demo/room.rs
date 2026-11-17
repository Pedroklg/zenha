//! Simulated rooms: join, unlock, mute, display name and listing.

use std::time::{Duration, Instant};

use zenha_core::RoomKey;
use zenha_protocol::RoomSummary;

use crate::ui_model::{ParticipantView, UiEvent};

use super::{
    DEMO_AVATAR_IDENTITY, DEMO_AVATAR_VERSION, DemoMicMeter, DemoRoom, DemoSession, FAILING_ROOM,
    SELF_IDENTITY, SELF_NAME,
};

impl DemoSession {
    /// Rooms whose password applies to `room`: itself and the parent room.
    pub(super) fn lock_owners(&self, room: &str) -> Vec<&DemoRoom> {
        let find = |id: &str| self.rooms_list.iter().find(|r| r.id == id);
        let own = find(room);
        let parent = own.and_then(|r| r.parent.as_deref()).and_then(find);
        [own, parent]
            .into_iter()
            .flatten()
            .filter(|r| r.password.is_some())
            .collect()
    }

    pub(super) fn needs_password(&self, room: &str) -> bool {
        let owners = self.lock_owners(room);
        !owners.is_empty() && !owners.iter().any(|o| self.unlocked.contains(&o.id))
    }

    pub(super) fn unlock(&mut self, room: &str, password: &str) {
        let owner = self
            .lock_owners(room)
            .into_iter()
            .find(|o| o.password.as_deref() == Some(password))
            .map(|o| o.id.clone());
        let Some(owner) = owner else {
            self.schedule(
                Duration::ZERO,
                UiEvent::RoomUnlockFailed {
                    message: "Senha da sala incorreta.".to_owned(),
                },
            );
            return;
        };
        self.unlocked.push(owner.clone());
        self.schedule(
            Duration::ZERO,
            UiEvent::RoomUnlocked {
                key: RoomKey(format!("demo-key-{owner}")),
                room: owner,
            },
        );
        let rooms = self.rooms();
        self.schedule(Duration::ZERO, UiEvent::RoomsUpdated(rooms));
    }

    pub(super) fn join(&mut self, room: String) {
        self.reset_share();
        self.scheduled.clear();
        self.chat_counter = 0;
        self.file_counter = 0;
        if self.needs_password(&room) {
            self.room = None;
            self.mic_meter = None;
            self.schedule(Duration::ZERO, UiEvent::Connecting);
            self.schedule(self.delays.join, UiEvent::RoomLocked { room });
            return;
        }
        if room == FAILING_ROOM {
            self.room = None;
            self.mic_meter = None;
            self.schedule(Duration::ZERO, UiEvent::Connecting);
            self.schedule(
                self.delays.join,
                UiEvent::Failed {
                    message: "Sala de demonstração que sempre falha.".to_owned(),
                },
            );
            return;
        }
        let mut me = Self::participant(SELF_IDENTITY, SELF_NAME);
        me.muted = self.muted;
        let mut ana = Self::participant(DEMO_AVATAR_IDENTITY, "Ana");
        ana.avatar_version = Some(DEMO_AVATAR_VERSION.to_owned());
        let bruno = Self::participant("demo-bruno", "Bruno");
        self.participants = vec![me.clone(), ana.clone()];
        self.room = Some(room.clone());
        self.mic_meter = (!self.delays.mic_level.is_zero()).then(|| DemoMicMeter {
            next_due: Instant::now() + self.delays.join,
            step: 0,
        });

        let join = self.delays.join;
        let late = join + self.delays.late_participant;
        self.schedule(Duration::ZERO, UiEvent::Connecting);
        self.schedule(
            join,
            UiEvent::Joined {
                room,
                local_identity: SELF_IDENTITY.to_owned(),
                participants: vec![me, ana],
            },
        );
        self.schedule(
            join,
            UiEvent::ChatHistory {
                messages: Self::initial_history(),
                has_more: true,
                before: None,
            },
        );
        let [lobby, started] = Self::webhook_posts();
        self.schedule(join + self.delays.reply, UiEvent::WebhookPosted(lobby));
        self.schedule(
            join + self.delays.reply * 2,
            UiEvent::WebhookPosted(started),
        );
        self.ana_sharing = true;
        self.schedule(
            join + self.delays.remote_share,
            UiEvent::RemoteScreenShareStarted {
                identity: "demo-ana".to_owned(),
            },
        );
        self.schedule(late, UiEvent::ParticipantJoined(bruno.clone()));
        let speaking = ParticipantView {
            speaking: true,
            ..bruno.clone()
        };
        self.schedule(
            late + Duration::from_millis(600),
            UiEvent::ParticipantUpdated(speaking),
        );
        self.schedule(
            late + Duration::from_millis(2400),
            UiEvent::ParticipantUpdated(bruno),
        );
        self.schedule(
            late + Duration::from_secs(6),
            UiEvent::ParticipantLeft {
                identity: "demo-bruno".to_owned(),
            },
        );
        self.schedule(late + Duration::from_secs(9), UiEvent::Reconnecting);
        self.schedule(late + Duration::from_secs(11), UiEvent::Reconnected);
    }

    pub(super) fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if self.room.is_none() {
            return;
        }
        if let Some(me) = self
            .participants
            .iter_mut()
            .find(|p| p.identity == SELF_IDENTITY)
        {
            me.muted = muted;
            let update = UiEvent::ParticipantUpdated(me.clone());
            self.schedule(Duration::ZERO, update);
        }
    }

    pub(super) fn set_display_name(&mut self, name: String) {
        if let Some(me) = self
            .participants
            .iter_mut()
            .find(|p| p.identity == SELF_IDENTITY)
        {
            me.name = name;
            let update = UiEvent::ParticipantUpdated(me.clone());
            self.schedule(Duration::ZERO, update);
        }
    }

    pub(super) fn rooms(&self) -> Vec<RoomSummary> {
        self.rooms_list
            .iter()
            .map(|room| {
                let participants = if self.room.as_deref() == Some(room.id.as_str()) {
                    self.participants.iter().map(|p| p.name.clone()).collect()
                } else {
                    room.idle_occupants.clone()
                };
                let locked = !self.lock_owners(&room.id).is_empty();
                let hidden = self.needs_password(&room.id);
                RoomSummary {
                    name: room.id.clone(),
                    title: room.title.clone(),
                    participant_count: participants.len(),
                    participants: if hidden { Vec::new() } else { participants },
                    locked,
                    unlocked: locked && !hidden,
                    parent: room.parent.clone(),
                }
            })
            .collect()
    }
}
