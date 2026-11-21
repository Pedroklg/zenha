//! `--demo` mode: simulated session with fake rooms, participants, chat and sharing.

mod chat;
mod commands;
mod fixtures;
mod room;
mod share;
#[cfg(test)]
mod tests;
mod webhooks;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use zenha_protocol::AdminWebhook;

use crate::core_session::TestFeed;
use crate::ui_model::{ParticipantView, SessionPort, UiCommand, UiEvent};

pub use fixtures::demo_avatar_image;

pub const SELF_IDENTITY: &str = "demo-self";
pub const SELF_NAME: &str = "Você";
pub const DEMO_SERVER_URL: &str = "https://demo.zenha.local";
pub const DEMO_PASSWORD: &str = "demo";
pub const DEMO_AVATAR_IDENTITY: &str = "demo-ana";
pub const DEMO_AVATAR_VERSION: &str = "demo";
const FAILING_ROOM: &str = "falha";
/// Password of the "VIP" room (and its child "Cantinho") in demo mode.
pub const DEMO_ROOM_PASSWORD: &str = "demo";
const REPLIES: [&str; 4] = [
    "Boa!",
    "Haha, verdade.",
    "Já estou chegando.",
    "Tá ouvindo bem?",
];
const DEMO_MIC_LEVELS: [i8; 30] = [
    -63, -61, -59, -62, -60, -58, -61, -59, -56, -49, -38, -28, -22, -26, -31, -24, -21, -27, -34,
    -25, -29, -37, -43, -48, -53, -57, -60, -62, -59, -61,
];

#[derive(Debug, Clone, Copy)]
pub struct DemoDelays {
    pub join: Duration,
    pub late_participant: Duration,
    pub reply: Duration,
    pub remote_share: Duration,
    pub mic_level: Duration,
}

impl Default for DemoDelays {
    fn default() -> Self {
        Self {
            join: Duration::from_millis(900),
            late_participant: Duration::from_secs(4),
            reply: Duration::from_millis(1200),
            remote_share: Duration::from_secs(2),
            mic_level: Duration::from_millis(100),
        }
    }
}

impl DemoDelays {
    #[cfg(test)]
    pub const fn instant() -> Self {
        Self {
            join: Duration::ZERO,
            late_participant: Duration::ZERO,
            reply: Duration::ZERO,
            remote_share: Duration::ZERO,
            mic_level: Duration::ZERO,
        }
    }
}

struct DemoMicMeter {
    next_due: Instant,
    step: u64,
}

fn demo_mic_reading(step: u64, noise_gate: Option<i8>) -> (i8, bool) {
    let level_db = DEMO_MIC_LEVELS[(step as usize) % DEMO_MIC_LEVELS.len()];
    let gate_open = noise_gate.is_none_or(|threshold| level_db >= threshold);
    (level_db, gate_open)
}

struct Scheduled {
    due: Instant,
    seq: u64,
    event: UiEvent,
}

#[derive(Default)]
struct DemoRoom {
    id: String,
    title: String,
    idle_occupants: Vec<String>,
    password: Option<String>,
    parent: Option<String>,
}

pub struct DemoSession {
    commands_tx: Sender<UiCommand>,
    commands_rx: Receiver<UiCommand>,
    delays: DemoDelays,
    scheduled: Vec<Scheduled>,
    next_seq: u64,
    room: Option<String>,
    rooms_list: Vec<DemoRoom>,
    unlocked: Vec<String>,
    room_counter: u64,
    admin_active: bool,
    webhooks: Vec<AdminWebhook>,
    webhook_counter: u64,
    participants: Vec<ParticipantView>,
    muted: bool,
    noise_gate: Option<i8>,
    mic_meter: Option<DemoMicMeter>,
    chat_counter: u64,
    file_counter: u64,
    input: Option<String>,
    output: Option<String>,
    share_preview: Option<TestFeed>,
    ana_sharing: bool,
    watch: Option<TestFeed>,
}

impl DemoSession {
    pub fn new() -> Self {
        chat::prepare_demo_image();
        Self::with_delays(DemoDelays::default())
    }

    pub fn with_delays(delays: DemoDelays) -> Self {
        let (commands_tx, commands_rx) = channel();
        Self {
            commands_tx,
            commands_rx,
            delays,
            scheduled: Vec::new(),
            next_seq: 0,
            room: None,
            rooms_list: vec![
                DemoRoom {
                    id: "geral".to_owned(),
                    title: "Geral".to_owned(),
                    idle_occupants: vec!["Ana".to_owned(), "Bruno".to_owned()],
                    ..DemoRoom::default()
                },
                DemoRoom {
                    id: "jogos".to_owned(),
                    title: "Jogos".to_owned(),
                    idle_occupants: vec!["Carla".to_owned()],
                    ..DemoRoom::default()
                },
                DemoRoom {
                    id: "musica".to_owned(),
                    title: "Música".to_owned(),
                    ..DemoRoom::default()
                },
                DemoRoom {
                    id: "vip".to_owned(),
                    title: "VIP".to_owned(),
                    idle_occupants: vec!["Duda".to_owned()],
                    password: Some(DEMO_ROOM_PASSWORD.to_owned()),
                    parent: None,
                },
                DemoRoom {
                    id: "cantinho".to_owned(),
                    title: "Cantinho".to_owned(),
                    parent: Some("vip".to_owned()),
                    ..DemoRoom::default()
                },
            ],
            unlocked: Vec::new(),
            room_counter: 0,
            admin_active: false,
            webhooks: Self::demo_webhooks(),
            webhook_counter: 0,
            participants: Vec::new(),
            muted: false,
            noise_gate: None,
            mic_meter: None,
            chat_counter: 0,
            file_counter: 0,
            input: None,
            output: None,
            share_preview: None,
            ana_sharing: false,
            watch: None,
        }
    }
    pub(super) fn schedule(&mut self, after: Duration, event: UiEvent) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.scheduled.push(Scheduled {
            due: Instant::now() + after,
            seq,
            event,
        });
    }

    pub(super) fn participant(identity: &str, name: &str) -> ParticipantView {
        ParticipantView {
            identity: identity.to_owned(),
            name: name.to_owned(),
            muted: false,
            deafened: false,
            speaking: false,
            avatar_version: None,
        }
    }
}

impl Default for DemoSession {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionPort for DemoSession {
    fn send(&self, command: UiCommand) {
        if self.commands_tx.send(command).is_err() {
            tracing::warn!("demo session command channel closed");
        }
    }

    fn try_recv(&mut self) -> Option<UiEvent> {
        while let Ok(command) = self.commands_rx.try_recv() {
            self.handle(command);
        }
        let now = Instant::now();
        if let Some(index) = self
            .scheduled
            .iter()
            .enumerate()
            .filter(|(_, s)| s.due <= now)
            .min_by_key(|(_, s)| (s.due, s.seq))
            .map(|(index, _)| index)
        {
            return Some(self.scheduled.swap_remove(index).event);
        }
        if self.room.is_some()
            && !self.delays.mic_level.is_zero()
            && let Some(meter) = &mut self.mic_meter
            && now >= meter.next_due
        {
            let (level_db, gate_open) = demo_mic_reading(meter.step, self.noise_gate);
            meter.step = meter.step.wrapping_add(1);
            meter.next_due = now + self.delays.mic_level;
            return Some(UiEvent::MicLevel {
                level_db,
                gate_open,
            });
        }
        None
    }
}
