//! Incoming webhooks in the Discord format and the message the server forwards to a room.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{ValidationError, check_len, validate_room_name};

/// Server message on topic `webhook` carrying a [`WebhookPost`].
///
/// Security: the client must only accept messages on this topic when they come from the server (no
/// sender participant), never from another participant.
pub const WEBHOOK_TOPIC: &str = "webhook";
pub const MAX_WEBHOOK_BODY_BYTES: usize = 16 * 1024;
pub const MAX_WEBHOOK_NAME_LEN: usize = 32;
pub const MAX_WEBHOOK_ID_LEN: usize = 32;
pub const MAX_WEBHOOKS_COUNT: usize = 20;
pub const MAX_WEBHOOK_EMBEDS: usize = 4;
pub const MAX_WEBHOOK_FIELDS: usize = 10;
pub const MAX_WEBHOOK_TITLE_LEN: usize = 256;
pub const MAX_WEBHOOK_TEXT_LEN: usize = 1024;
pub const MAX_WEBHOOK_URL_LEN: usize = 512;
/// How long the server keeps integration posts for the room history.
pub const WEBHOOK_POST_TTL_MS: u64 = 12 * 60 * 60 * 1000;
/// Most recent integration posts returned with a room history.
pub const MAX_HISTORY_WEBHOOK_POSTS: usize = 50;

/// Body accepted by `POST /hooks/{id}/{token}`: the subset of a Discord webhook the client shows.
/// Unknown fields (`color`, `image`, `username`, ...) are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct DiscordWebhookBody {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub embeds: Vec<DiscordEmbed>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct DiscordEmbed {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub fields: Vec<DiscordField>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct DiscordField {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// Message sent by an integration, delivered to whoever is in the room and never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookPost {
    pub id: String,
    /// Name of the webhook set by the administrator, never taken from the request body.
    pub name: String,
    pub sent_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embeds: Vec<WebhookEmbed>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookEmbed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Only `https://` URLs are kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<WebhookField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookField {
    pub name: String,
    pub value: String,
}

impl DiscordWebhookBody {
    /// Trims texts to the limits and drops extra embeds, extra fields and non-`https` URLs.
    /// Returns `None` when nothing is left to show.
    pub fn into_post(self, id: String, name: String, sent_at_unix_ms: u64) -> Option<WebhookPost> {
        let embeds: Vec<WebhookEmbed> = self
            .embeds
            .into_iter()
            .take(MAX_WEBHOOK_EMBEDS)
            .map(|embed| WebhookEmbed {
                title: clip(embed.title, MAX_WEBHOOK_TITLE_LEN),
                url: embed.url.filter(|url| is_https_url(url)),
                description: clip(embed.description, MAX_WEBHOOK_TEXT_LEN),
                fields: embed
                    .fields
                    .into_iter()
                    .filter_map(|field| {
                        Some(WebhookField {
                            name: clip(Some(field.name), MAX_WEBHOOK_TITLE_LEN).unwrap_or_default(),
                            value: clip(Some(field.value), MAX_WEBHOOK_TEXT_LEN)?,
                        })
                    })
                    .take(MAX_WEBHOOK_FIELDS)
                    .collect(),
            })
            .filter(|embed| !embed.is_empty())
            .collect();
        let content = clip(self.content, MAX_WEBHOOK_TEXT_LEN);
        if content.is_none() && embeds.is_empty() {
            return None;
        }
        Some(WebhookPost {
            id,
            name,
            sent_at_unix_ms,
            content,
            embeds,
        })
    }
}

impl WebhookEmbed {
    fn is_empty(&self) -> bool {
        self.title.is_none() && self.description.is_none() && self.fields.is_empty()
    }
}

impl WebhookPost {
    /// Checks the limits on the receiving side; a post from an older server may be smaller, never larger.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_webhook_id(&self.id)?;
        validate_webhook_name(&self.name)?;
        if self.embeds.len() > MAX_WEBHOOK_EMBEDS {
            return Err(ValidationError::TooLong {
                max: MAX_WEBHOOK_EMBEDS,
            });
        }
        if let Some(content) = &self.content {
            check_len(content, MAX_WEBHOOK_TEXT_LEN)?;
        }
        for embed in &self.embeds {
            if embed.fields.len() > MAX_WEBHOOK_FIELDS {
                return Err(ValidationError::TooLong {
                    max: MAX_WEBHOOK_FIELDS,
                });
            }
            if let Some(title) = &embed.title {
                check_len(title, MAX_WEBHOOK_TITLE_LEN)?;
            }
            if let Some(url) = &embed.url
                && !is_https_url(url)
            {
                return Err(ValidationError::InvalidCharacter);
            }
            if let Some(description) = &embed.description {
                check_len(description, MAX_WEBHOOK_TEXT_LEN)?;
            }
            for field in &embed.fields {
                if field.name.chars().count() > MAX_WEBHOOK_TITLE_LEN {
                    return Err(ValidationError::TooLong {
                        max: MAX_WEBHOOK_TITLE_LEN,
                    });
                }
                check_len(&field.value, MAX_WEBHOOK_TEXT_LEN)?;
            }
        }
        if self.content.is_none() && self.embeds.is_empty() {
            return Err(ValidationError::Empty);
        }
        Ok(())
    }
}

/// Webhook as listed by the administrator. The token is never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminWebhook {
    pub id: String,
    pub room: String,
    pub name: String,
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWebhookRequest {
    pub room: String,
    pub name: String,
}

impl CreateWebhookRequest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_room_name(&self.room)?;
        validate_webhook_name(&self.name)
    }
}

/// Response to `POST /admin/webhooks`, the only time the token is revealed.
///
/// The server sends `url` as a path (`/hooks/{id}/{token}`); the client joins it with the server
/// address before showing it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedWebhook {
    pub webhook: AdminWebhook,
    pub url: String,
}

impl fmt::Debug for CreatedWebhook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedWebhook")
            .field("webhook", &self.webhook)
            .field("url", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhooksResponse {
    pub webhooks: Vec<AdminWebhook>,
}

/// Webhook name: 1 to 32 characters after trimming, no control characters.
pub fn validate_webhook_name(name: &str) -> Result<(), ValidationError> {
    let trimmed = name.trim();
    check_len(trimmed, MAX_WEBHOOK_NAME_LEN)?;
    if trimmed.chars().any(char::is_control) {
        return Err(ValidationError::InvalidCharacter);
    }
    Ok(())
}

/// Webhook id: 1 to 32 ASCII letters or digits.
pub fn validate_webhook_id(id: &str) -> Result<(), ValidationError> {
    check_len(id, MAX_WEBHOOK_ID_LEN)?;
    if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(ValidationError::InvalidCharacter);
    }
    Ok(())
}

fn is_https_url(url: &str) -> bool {
    url.starts_with("https://")
        && url.len() <= MAX_WEBHOOK_URL_LEN
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn clip(text: Option<String>, max: usize) -> Option<String> {
    let text = text?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(max).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOBBY: &str = r#"{"embeds":[{"title":"Clique aqui para abrir a lobby","color":"2391737","url":"https://gamersclub.com.br/j/123/abc","fields":[{"name":"Tipo da sala:","value":"FECHADA"},{"name":"Membros:","value":"ana | 15 | KDR: 1.2 \n"}]}]}"#;
    const MATCH: &str = r#"{"embeds":[{"color":"2391737","fields":[{"name":"IP da partida:","value":"connect 1.2.3.4:27015;password xyz \n[Conectar ao servidor](steam://connect/1.2.3.4:27015/xyz)"},{"name":"Mapa:","value":"de_mirage"}],"image":{"url":"https://example.com/m.png"}}]}"#;
    const TEST: &str = r#"{"embeds":[{"title":"Gamers Club Booster","color":"4961603","fields":[{"name":"Status","value":"OK"}]}]}"#;

    fn post(json: &str) -> Option<WebhookPost> {
        let body: DiscordWebhookBody = serde_json::from_str(json).unwrap();
        body.into_post("abc123".to_owned(), "Gamers Club".to_owned(), 7)
    }

    #[test]
    fn extension_bodies_become_posts() {
        let lobby = post(LOBBY).unwrap();
        assert_eq!(
            lobby.embeds[0].url.as_deref(),
            Some("https://gamersclub.com.br/j/123/abc")
        );
        assert_eq!(lobby.embeds[0].fields.len(), 2);
        let game = post(MATCH).unwrap();
        assert!(
            game.embeds[0].fields[0]
                .value
                .starts_with("connect 1.2.3.4:27015;password xyz")
        );
        assert_eq!(game.embeds[0].title, None);
        let test = post(TEST).unwrap();
        assert_eq!(test.embeds[0].title.as_deref(), Some("Gamers Club Booster"));
        for post in [lobby, game, test] {
            assert_eq!(post.validate(), Ok(()));
        }
    }

    #[test]
    fn empty_bodies_and_unsafe_urls_are_dropped() {
        assert_eq!(post(r#"{"content":"  ","embeds":[{}]}"#), None);
        let only_url =
            post(r#"{"content":"x","embeds":[{"url":"javascript:alert(1)","title":"t"}]}"#);
        assert_eq!(only_url.unwrap().embeds[0].url, None);
    }

    #[test]
    fn long_texts_and_extra_items_are_clipped() {
        let fields: Vec<String> = (0..20)
            .map(|_| format!(r#"{{"name":"n","value":"{}"}}"#, "v".repeat(5000)))
            .collect();
        let json = format!(r#"{{"embeds":[{{"fields":[{}]}}]}}"#, fields.join(","));
        let post = post(&json).unwrap();
        assert_eq!(post.embeds[0].fields.len(), MAX_WEBHOOK_FIELDS);
        assert_eq!(
            post.embeds[0].fields[0].value.chars().count(),
            MAX_WEBHOOK_TEXT_LEN
        );
        assert_eq!(post.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_oversized_posts() {
        let mut post = post(TEST).unwrap();
        post.embeds[0].fields[0].value = "v".repeat(MAX_WEBHOOK_TEXT_LEN + 1);
        assert!(post.validate().is_err());
    }

    #[test]
    fn webhook_types_round_trip_through_json() {
        let post = post(LOBBY).unwrap();
        let json = serde_json::to_string(&post).unwrap();
        assert_eq!(serde_json::from_str::<WebhookPost>(&json).unwrap(), post);
        let created = CreatedWebhook {
            webhook: AdminWebhook {
                id: "abc".to_owned(),
                room: "geral".to_owned(),
                name: "GC".to_owned(),
                created_at_unix_ms: 1,
            },
            url: "/hooks/abc/secret".to_owned(),
        };
        let json = serde_json::to_string(&created).unwrap();
        assert_eq!(
            serde_json::from_str::<CreatedWebhook>(&json).unwrap(),
            created
        );
        assert!(!format!("{created:?}").contains("secret"));
        let list = WebhooksResponse {
            webhooks: vec![created.webhook.clone()],
        };
        let json = serde_json::to_string(&list).unwrap();
        assert_eq!(
            serde_json::from_str::<WebhooksResponse>(&json).unwrap(),
            list
        );
    }

    #[test]
    fn names_and_ids_are_validated() {
        assert_eq!(validate_webhook_name(" GC "), Ok(()));
        assert!(validate_webhook_name("").is_err());
        assert!(validate_webhook_id("abc-1").is_err());
        assert_eq!(validate_webhook_id("abc1"), Ok(()));
    }
}
