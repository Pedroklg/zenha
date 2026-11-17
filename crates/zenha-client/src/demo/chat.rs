//! Simulated chat: messages, history, files and automatic replies.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use zenha_protocol::{WebhookEmbed, WebhookField, WebhookPost};

use crate::ui_model::{ChatMessageView, FileTransferView, TransferStateView, UiEvent};

use super::{DemoSession, REPLIES, SELF_IDENTITY, SELF_NAME};

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
            },
            None => UiEvent::ChatHistory {
                messages: Self::initial_history(),
                has_more: true,
                before: None,
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
