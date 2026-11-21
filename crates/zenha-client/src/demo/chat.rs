//! Simulated chat: messages, history, files and automatic replies.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use zenha_protocol::{WebhookEmbed, WebhookField, WebhookPost};

use crate::ui_model::{ChatMessageView, FileTransferView, TransferStateView, UiEvent};

use super::{DemoSession, REPLIES, SELF_IDENTITY, SELF_NAME};

const DEMO_IMAGE_NAME: &str = "print-da-partida.png";

fn demo_image_path() -> PathBuf {
    std::env::temp_dir()
        .join("zenha-demo")
        .join(DEMO_IMAGE_NAME)
}

/// Draws the picture Ana sends in the demo, off the UI thread, so the demo needs no asset file.
pub(super) fn prepare_demo_image() {
    let path = demo_image_path();
    let _ = std::thread::Builder::new()
        .name("demo-image".to_owned())
        .spawn(move || {
            if path.exists() {
                return;
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(err) = draw_demo_image().save(&path) {
                tracing::debug!(%err, "could not write the demo image");
            }
        });
}

/// A dusk landscape: sky gradient, sun and two mountain ridges.
fn draw_demo_image() -> image::RgbImage {
    const WIDTH: u32 = 960;
    const HEIGHT: u32 = 540;
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| {
        let t = t.clamp(0.0, 1.0);
        image::Rgb([0, 1, 2].map(|i| (a[i] + (b[i] - a[i]) * t) as u8))
    };
    image::RgbImage::from_fn(WIDTH, HEIGHT, |x, y| {
        let fx = x as f32 / WIDTH as f32;
        let fy = y as f32 / HEIGHT as f32;
        let far = 0.62 - 0.10 * (fx * 9.0).sin() - 0.04 * (fx * 23.0).sin();
        let near = 0.80 - 0.07 * (fx * 5.0 + 1.0).sin() - 0.03 * (fx * 17.0).sin();
        let sun_distance = (((fx - 0.68) * 16.0 / 9.0).powi(2) + (fy - 0.5).powi(2)).sqrt();
        if fy > near {
            mix([30.0, 26.0, 52.0], [12.0, 14.0, 28.0], (fy - near) * 4.0)
        } else if fy > far {
            mix([92.0, 64.0, 120.0], [44.0, 38.0, 78.0], (fy - far) * 5.0)
        } else if sun_distance < 0.09 {
            mix(
                [255.0, 236.0, 180.0],
                [255.0, 190.0, 120.0],
                sun_distance / 0.09,
            )
        } else {
            mix([36.0, 48.0, 112.0], [250.0, 140.0, 96.0], fy / 0.62)
        }
    })
}

pub(super) fn demo_image_transfer() -> FileTransferView {
    FileTransferView {
        id: "demo-image".to_owned(),
        sender: "Ana".to_owned(),
        name: DEMO_IMAGE_NAME.to_owned(),
        size: 48 * 1024,
        is_image: true,
        outgoing: false,
        sent_at_unix_ms: DemoSession::now_unix_ms(),
        state: TransferStateView::Done {
            path: demo_image_path(),
        },
    }
}

fn is_image_name(name: &str) -> bool {
    zenha_core::is_image_mime(zenha_core::mime_for_name(name))
}

impl DemoSession {
    pub(super) fn chat(&mut self, text: String, save: bool) {
        if self.room.is_none() {
            return;
        }
        self.chat_counter += 1;
        let echo = Self::chat_message(
            format!("demo-{}", self.chat_counter * 2 - 1),
            SELF_IDENTITY,
            SELF_NAME,
            text,
            save,
            Self::now_unix_ms(),
        );
        self.schedule(Duration::ZERO, UiEvent::ChatReceived(echo));
        if self.chat_counter.is_multiple_of(2) {
            let index = usize::try_from(self.chat_counter / 2).unwrap_or(0) % REPLIES.len();
            let reply = Self::chat_message(
                format!("demo-{}", self.chat_counter * 2),
                "demo-ana",
                "Ana",
                REPLIES[index].to_owned(),
                false,
                Self::now_unix_ms(),
            );
            self.schedule(self.delays.reply, UiEvent::ChatReceived(reply));
        }
    }

    pub(super) fn load_chat_history(&mut self, before: Option<String>) {
        if self.room.is_none() {
            return;
        }
        let delay = self.delays.reply.min(Duration::from_millis(250));
        let event = match before {
            Some(cursor) => UiEvent::ChatHistory {
                messages: Self::older_history(),
                has_more: false,
                before: Some(cursor),
                webhook_posts: Vec::new(),
            },
            None => UiEvent::ChatHistory {
                messages: Self::initial_history(),
                has_more: true,
                before: None,
                webhook_posts: Vec::new(),
            },
        };
        self.schedule(delay, event);
    }

    pub(super) fn delete_chat(&mut self, id: String) {
        if self.room.is_none() {
            return;
        }
        self.schedule(
            Duration::ZERO,
            UiEvent::ChatDeleted {
                id,
                sender: SELF_IDENTITY.to_owned(),
            },
        );
    }

    pub(super) fn send_file(&mut self, path: PathBuf) {
        if self.room.is_none() {
            return;
        }
        self.file_counter += 1;
        let size = std::fs::metadata(&path).map_or(1024 * 1024, |m| m.len());
        let name = path.file_name().map_or_else(
            || "arquivo".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let sent_at_unix_ms = Self::now_unix_ms();
        let view = |state| FileTransferView {
            id: format!("demo-file-{}", self.file_counter),
            sender: SELF_NAME.to_owned(),
            name: name.clone(),
            size,
            is_image: is_image_name(&name),
            outgoing: true,
            sent_at_unix_ms,
            state,
        };
        let started = view(TransferStateView::InProgress { bytes: size / 2 });
        let finished = view(TransferStateView::Done { path });
        self.schedule(Duration::ZERO, UiEvent::FileTransfer(started));
        self.schedule(self.delays.reply, UiEvent::FileTransfer(finished));
    }

    pub(super) fn webhook_posts() -> [WebhookPost; 2] {
        let now = Self::now_unix_ms();
        let field = |name: &str, value: &str| WebhookField {
            name: name.to_owned(),
            value: value.to_owned(),
        };
        let embed = |title: Option<&str>, url: Option<&str>, fields| WebhookEmbed {
            title: title.map(str::to_owned),
            url: url.map(str::to_owned),
            description: None,
            fields,
        };
        let post = |id: &str, embed| WebhookPost {
            id: id.to_owned(),
            name: "Gamers Club".to_owned(),
            sent_at_unix_ms: now,
            content: None,
            embeds: vec![embed],
        };
        [
            post(
                "demolobby",
                embed(
                    Some("Clique aqui para abrir a lobby"),
                    Some("https://gamersclub.com.br/j/123/abc"),
                    vec![
                        field("Tipo da sala:", "FECHADA"),
                        field("Admin da sala:", "ana | 15"),
                        field("Membros:", "ana | 15 | KDR: 1.2"),
                    ],
                ),
            ),
            post(
                "demomatch",
                embed(
                    None,
                    None,
                    vec![
                        field(
                            "Time ana - 15",
                            "15 - ana \n16 - carla \n14 - pedro \n17 - lucas \n15 - bruno",
                        ),
                        field(
                            "Time bia - 17",
                            "17 - bia \n18 - duda \n16 - rafa \n15 - gio \n17 - teo",
                        ),
                        field(
                            "IP da partida:",
                            "connect 1.2.3.4:27015;password xyz \n[Conectar ao servidor](steam://connect/1.2.3.4:27015/xyz)",
                        ),
                        field("Mapa:", "de_mirage"),
                    ],
                ),
            ),
        ]
    }

    pub(super) fn now_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }

    pub(super) fn chat_message(
        id: String,
        sender_id: &str,
        sender: &str,
        text: String,
        saved: bool,
        sent_at_unix_ms: u64,
    ) -> ChatMessageView {
        ChatMessageView {
            id,
            sender_id: sender_id.to_owned(),
            sender: sender.to_owned(),
            sent_at_unix_ms,
            text,
            saved,
        }
    }

    pub(super) fn initial_history() -> Vec<ChatMessageView> {
        let now = Self::now_unix_ms();
        vec![
            Self::chat_message(
                "demo-hist-3".to_owned(),
                "demo-ana",
                "Ana",
                "Alguém topa chamada mais tarde?".to_owned(),
                true,
                now.saturating_sub(30 * 60_000),
            ),
            Self::chat_message(
                "demo-hist-4".to_owned(),
                SELF_IDENTITY,
                SELF_NAME,
                "Link da pauta já está no chat.".to_owned(),
                true,
                now.saturating_sub(15 * 60_000),
            ),
        ]
    }

    pub(super) fn older_history() -> Vec<ChatMessageView> {
        let now = Self::now_unix_ms();
        vec![
            Self::chat_message(
                "demo-hist-1".to_owned(),
                SELF_IDENTITY,
                SELF_NAME,
                "Bem-vindo à sala de demonstração.".to_owned(),
                true,
                now.saturating_sub(120 * 60_000),
            ),
            Self::chat_message(
                "demo-hist-2".to_owned(),
                "demo-ana",
                "Ana",
                "Combinado!".to_owned(),
                true,
                now.saturating_sub(60 * 60_000),
            ),
        ]
    }
}
