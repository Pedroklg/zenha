//! Simulated webhook administration.

use std::time::Duration;

use zenha_protocol::{AdminWebhook, CreatedWebhook};

use crate::ui_model::UiEvent;

use super::{DEMO_SERVER_URL, DemoSession};

impl DemoSession {
    pub(super) fn demo_webhooks() -> Vec<AdminWebhook> {
        let now = Self::now_unix_ms();
        vec![
            AdminWebhook {
                id: "demowh1".to_owned(),
                room: "geral".to_owned(),
                name: "Gamers Club".to_owned(),
                created_at_unix_ms: now.saturating_sub(86_400_000),
            },
            AdminWebhook {
                id: "demowh2".to_owned(),
                room: "jogos".to_owned(),
                name: "Torneio".to_owned(),
                created_at_unix_ms: now.saturating_sub(3_600_000),
            },
        ]
    }

    pub(super) fn list_webhooks(&mut self) {
        let webhooks = self.webhooks.clone();
        self.schedule(Duration::ZERO, UiEvent::AdminWebhooks(webhooks));
    }

    pub(super) fn create_webhook(&mut self, room: String, name: String) {
        if !self.rooms_list.iter().any(|r| r.id == room) {
            self.schedule(
                Duration::ZERO,
                UiEvent::AdminFailed {
                    message: "Sala, participante ou mensagem não encontrado no servidor."
                        .to_owned(),
                },
            );
            return;
        }
        self.webhook_counter += 1;
        let id = format!("demonew{}", self.webhook_counter);
        let webhook = AdminWebhook {
            id: id.clone(),
            room,
            name,
            created_at_unix_ms: Self::now_unix_ms(),
        };
        self.webhooks.push(webhook.clone());
        let created = CreatedWebhook {
            webhook,
            url: format!("{DEMO_SERVER_URL}/hooks/{id}/demo-token-{id}"),
        };
        self.schedule(Duration::ZERO, UiEvent::AdminWebhookCreated(created));
    }

    pub(super) fn delete_webhook(&mut self, id: String) {
        let before = self.webhooks.len();
        self.webhooks.retain(|w| w.id != id);
        if self.webhooks.len() == before {
            self.schedule(
                Duration::ZERO,
                UiEvent::AdminFailed {
                    message: "Sala, participante ou mensagem não encontrado no servidor."
                        .to_owned(),
                },
            );
            return;
        }
        self.schedule(Duration::ZERO, UiEvent::AdminWebhookDeleted { id });
    }
}
