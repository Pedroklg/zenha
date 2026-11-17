//! Webhooks subsection of the admin settings.

use eframe::egui::{self, CornerRadius, Margin, RichText, Stroke};
use zenha_protocol::{MAX_WEBHOOK_NAME_LEN, RoomSummary};

use super::{COMBO_WIDTH, CONTROL_GAP, GROUP_GAP, INVITE_INPUT_WIDTH, ROW_GAP};
use crate::state::{AdminState, CreatedWebhookNotice, UserAction, room_display_title};
use crate::theme;

pub(super) fn webhooks_section(
    ui: &mut egui::Ui,
    admin: &mut AdminState,
    rooms: &[RoomSummary],
    actions: &mut Vec<UserAction>,
) {
    ui.add_space(GROUP_GAP);
    ui.label(RichText::new("Webhooks de integrações").font(theme::semibold(14.0)));
    ui.add_space(CONTROL_GAP);
    ui.weak("Mensagens de integrações, como o GamersClub Booster, aparecem no chat da sala escolhida.");
    ui.add_space(ROW_GAP);

    if let Some(created) = &admin.webhooks.created {
        created_box(ui, created, actions);
        ui.add_space(ROW_GAP);
    }

    if admin.webhooks.items.is_empty() {
        ui.weak("Nenhum webhook criado.");
    }
    for webhook in &admin.webhooks.items {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = ROW_GAP;
            ui.label(RichText::new(&webhook.name).font(theme::semibold(13.0)));
            ui.weak(format!("· {}", room_display_title(rooms, &webhook.room)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Revogar")
                    .on_hover_text(format!("Revogar o webhook {}", webhook.name))
                    .clicked()
                {
                    actions.push(UserAction::OpenRevokeWebhook(webhook.id.clone()));
                }
            });
        });
    }

    ui.add_space(ROW_GAP);
    create_form(ui, admin, rooms, actions);
}

fn created_box(ui: &mut egui::Ui, created: &CreatedWebhookNotice, actions: &mut Vec<UserAction>) {
    egui::Frame::new()
        .fill(theme::CONTROL)
        .stroke(Stroke::new(1.0, theme::ACCENT))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                RichText::new(format!("Webhook \"{}\" criado", created.name))
                    .font(theme::semibold(13.0))
                    .color(theme::ACCENT_TEXT),
            );
            ui.add_space(CONTROL_GAP);
            ui.add(
                egui::Label::new(RichText::new(&created.url.0).monospace().color(theme::TEXT))
                    .wrap(),
            );
            ui.add_space(CONTROL_GAP);
            ui.colored_label(
                theme::STATUS_PENDING,
                "Guarde esta URL: ela não aparece de novo. Cole na aba Discord do GamersClub Booster.",
            );
            ui.add_space(CONTROL_GAP);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = ROW_GAP;
                if ui.add(theme::accent_button("Copiar")).clicked() {
                    ui.ctx().copy_text(created.url.0.clone());
                }
                if ui.button("Fechar").clicked() {
                    actions.push(UserAction::DismissCreatedWebhook);
                }
            });
        });
}

fn create_form(
    ui: &mut egui::Ui,
    admin: &mut AdminState,
    rooms: &[RoomSummary],
    actions: &mut Vec<UserAction>,
) {
    ui.label("Novo webhook");
    ui.add_space(CONTROL_GAP);
    let selected = admin.webhooks.selected_room(rooms).map(str::to_owned);
    let selected_text = selected
        .as_deref()
        .map_or("Nenhuma sala", |id| room_display_title(rooms, id))
        .to_owned();
    egui::ComboBox::from_id_salt("webhook_room")
        .selected_text(selected_text)
        .width(COMBO_WIDTH)
        .show_ui(ui, |ui| {
            for room in rooms {
                let is_selected = selected.as_deref() == Some(room.name.as_str());
                if ui
                    .selectable_label(is_selected, room.display_title())
                    .clicked()
                {
                    admin.webhooks.new_room = Some(room.name.clone());
                }
            }
        });
    ui.add_space(CONTROL_GAP);
    let name = ui.add(
        egui::TextEdit::singleline(&mut admin.webhooks.new_name)
            .char_limit(MAX_WEBHOOK_NAME_LEN)
            .desired_width(INVITE_INPUT_WIDTH)
            .hint_text("Nome do webhook"),
    );
    let enter = name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.add_space(CONTROL_GAP);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = ROW_GAP;
        let can_create = !admin.webhooks.creating && selected.is_some();
        if ui
            .add_enabled(can_create, egui::Button::new("Criar webhook"))
            .clicked()
            || (can_create && enter)
        {
            actions.push(UserAction::CreateWebhook);
        }
        if admin.webhooks.creating {
            ui.spinner();
        }
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use zenha_protocol::{AdminWebhook, CreatedWebhook};

    use super::*;
    use crate::state::SecretText;

    const URL: &str = "https://voz.exemplo/hooks/n1/token-secreto";

    struct View {
        admin: AdminState,
        rooms: Vec<RoomSummary>,
        actions: Vec<UserAction>,
    }

    fn rooms() -> Vec<RoomSummary> {
        vec![
            RoomSummary {
                name: "geral".to_owned(),
                title: "Geral".to_owned(),
                ..RoomSummary::default()
            },
            RoomSummary {
                name: "jogos".to_owned(),
                title: "Jogos".to_owned(),
                ..RoomSummary::default()
            },
        ]
    }

    fn harness(admin: AdminState) -> Harness<'static, View> {
        Harness::builder()
            .with_size(egui::Vec2::new(600.0, 700.0))
            .with_accessibility_check(false)
            .with_context_setup(theme::install)
            .build_ui_state(
                |ui, view: &mut View| {
                    webhooks_section(ui, &mut view.admin, &view.rooms, &mut view.actions);
                },
                View {
                    admin,
                    rooms: rooms(),
                    actions: Vec::new(),
                },
            )
    }

    fn webhook(id: &str, room: &str, name: &str) -> AdminWebhook {
        AdminWebhook {
            id: id.to_owned(),
            room: room.to_owned(),
            name: name.to_owned(),
            created_at_unix_ms: 1,
        }
    }

    #[test]
    fn list_shows_name_and_room_title_and_revoke_emits_the_id() {
        let mut admin = AdminState::default();
        admin.webhooks.items = vec![webhook("a1", "jogos", "Torneio")];
        let mut harness = harness(admin);
        harness.run_steps(2);

        harness.get_by_label("Torneio");
        harness.get_by_label("· Jogos");
        harness.get_by_label("Revogar").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().actions,
            vec![UserAction::OpenRevokeWebhook("a1".to_owned())]
        );
    }

    #[test]
    fn created_box_shows_url_and_warning_and_copy_and_close_work() {
        let mut admin = AdminState::default();
        admin.webhooks.created = Some(CreatedWebhookNotice {
            id: "n1".to_owned(),
            name: "Gamers Club".to_owned(),
            room: "geral".to_owned(),
            url: SecretText(URL.to_owned()),
        });
        let mut harness = harness(admin);
        harness.run_steps(2);

        harness.get_by_label(URL);
        harness.get_by_label(
            "Guarde esta URL: ela não aparece de novo. Cole na aba Discord do GamersClub Booster.",
        );
        harness.get_by_label("Copiar").click();
        harness.step();
        let copied = harness
            .output()
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == URL));
        assert!(copied);

        harness.get_by_label("Fechar").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().actions,
            vec![UserAction::DismissCreatedWebhook]
        );
    }

    #[test]
    fn without_a_created_url_the_box_is_absent_and_create_button_works() {
        let mut harness = harness(AdminState::default());
        harness.run_steps(2);
        assert!(harness.query_by_label("Copiar").is_none());
        harness.get_by_label("Nenhum webhook criado.");

        harness.get_by_label("Criar webhook").click();
        harness.run_steps(2);
        assert_eq!(harness.state().actions, vec![UserAction::CreateWebhook]);
    }
}
