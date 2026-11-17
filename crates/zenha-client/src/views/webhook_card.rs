//! Card for a webhook post in the chat.

use eframe::egui::{self, CornerRadius, Margin, RichText, Stroke};
use zenha_protocol::{WebhookEmbed, WebhookField, WebhookPost};

use super::linked_text::linked_text;
use super::links::connect_command;
use crate::format::{DISPLAY_UTC_OFFSET_MINUTES, format_hhmm};
use crate::theme;

pub const BADGE_LABEL: &str = "integração";

pub fn card(ui: &mut egui::Ui, post: &WebhookPost) {
    ui.push_id(("webhook", &post.id, post.sent_at_unix_ms), |ui| {
        header(ui, post);
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme::CONTROL)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                if let Some(content) = &post.content {
                    linked_text(ui, content, theme::TEXT_SECONDARY);
                }
                for embed in &post.embeds {
                    embed_block(ui, embed);
                }
            });
    });
}

fn header(ui: &mut egui::Ui, post: &WebhookPost) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(&post.name)
                .font(theme::semibold(14.0))
                .color(theme::ACCENT_TEXT),
        );
        egui::Frame::new()
            .fill(theme::PANEL_DARK)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::symmetric(5, 1))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(BADGE_LABEL)
                        .size(10.5)
                        .color(theme::TEXT_WEAK),
                );
            });
        ui.weak(format_hhmm(
            post.sent_at_unix_ms,
            DISPLAY_UTC_OFFSET_MINUTES,
        ));
    });
}

fn embed_block(ui: &mut egui::Ui, embed: &WebhookEmbed) {
    if let Some(title) = &embed.title {
        let text = RichText::new(title).font(theme::semibold(13.0));
        match &embed.url {
            Some(url) => {
                ui.hyperlink_to(text, url);
            }
            None => {
                ui.label(text.color(theme::TEXT));
            }
        }
    }
    if let Some(description) = &embed.description {
        linked_text(ui, description, theme::TEXT_SECONDARY);
    }
    for field in &embed.fields {
        field_block(ui, field);
    }
}

fn field_block(ui: &mut egui::Ui, field: &WebhookField) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        if !field.name.is_empty() {
            ui.label(
                RichText::new(&field.name)
                    .font(theme::semibold(12.0))
                    .color(theme::TEXT),
            );
        }
        linked_text(ui, &field.value, theme::TEXT_SECONDARY);
        if let Some(connect) = connect_command(&field.value) {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let copy = egui::Button::new(
                    egui::RichText::new("Copiar")
                        .font(theme::semibold(14.0))
                        .color(theme::TEXT),
                )
                .corner_radius(egui::CornerRadius::same(8));
                if ui.add(copy).clicked() {
                    ui.ctx().copy_text(connect.command.clone());
                }
                if ui.add(theme::accent_button("Conectar")).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::same_tab(&connect.url));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    const MATCH_VALUE: &str = "connect 1.2.3.4:27015;password xyz \n[Conectar ao servidor](steam://connect/1.2.3.4:27015/xyz)";

    fn match_post() -> WebhookPost {
        WebhookPost {
            id: "abc123".to_owned(),
            name: "Gamers Club".to_owned(),
            sent_at_unix_ms: 7,
            content: None,
            embeds: vec![WebhookEmbed {
                title: Some("Partida pronta".to_owned()),
                url: Some("https://gamersclub.com.br/j/1".to_owned()),
                description: None,
                fields: vec![
                    WebhookField {
                        name: "IP da partida:".to_owned(),
                        value: MATCH_VALUE.to_owned(),
                    },
                    WebhookField {
                        name: "Mapa:".to_owned(),
                        value: "de_mirage".to_owned(),
                    },
                ],
            }],
        }
    }

    fn harness(post: WebhookPost) -> Harness<'static, WebhookPost> {
        Harness::builder()
            .with_size(egui::Vec2::new(320.0, 400.0))
            .with_accessibility_check(false)
            .with_context_setup(theme::install)
            .build_ui_state(|ui, post| card(ui, post), post)
    }

    #[test]
    fn card_shows_title_badge_field_and_connect_button() {
        let mut harness = harness(match_post());
        harness.run_steps(2);

        harness.get_by_label("Gamers Club");
        harness.get_by_label(BADGE_LABEL);
        harness.get_by_label("Partida pronta");
        harness.get_by_label("IP da partida:");
        harness.get_by_label("Mapa:");
        harness.get_by_label("Copiar");
        harness.get_by_label("Conectar");
    }

    #[test]
    fn connect_button_opens_the_steam_url_in_the_same_tab() {
        let mut harness = harness(match_post());
        harness.run_steps(2);
        harness.get_by_label("Conectar").click();
        harness.step();

        let opened = harness
            .output()
            .platform_output
            .commands
            .iter()
            .any(|command| {
                matches!(
                    command,
                    egui::OutputCommand::OpenUrl(open)
                        if open.url == "steam://connect/1.2.3.4:27015/xyz" && !open.new_tab
                )
            });
        assert!(opened);
    }

    #[test]
    fn copy_button_copies_the_normalized_command() {
        let mut harness = harness(match_post());
        harness.run_steps(2);
        harness.get_by_label("Copiar").click();
        harness.step();

        let copied = harness
            .output()
            .platform_output
            .commands
            .iter()
            .any(|command| {
                matches!(
                    command,
                    egui::OutputCommand::CopyText(text)
                        if text == "connect 1.2.3.4:27015;password xyz"
                )
            });
        assert!(copied);
    }

    #[test]
    fn card_without_connect_command_has_no_connect_buttons() {
        let mut post = match_post();
        post.embeds[0].fields.remove(0);
        let mut harness = harness(post);
        harness.run_steps(2);

        assert!(harness.query_by_label("Conectar").is_none());
        assert!(harness.query_by_label("Copiar").is_none());
    }
}
