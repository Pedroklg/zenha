//! Diálogos da sala: senha, administração e confirmação de atualização.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};
use zenha_protocol::{MAX_ROOM_PASSWORD_LEN, MAX_ROOM_TITLE_LEN, RoomSummary};

use crate::state::{AdminDialog, RoomPasswordDialog, UserAction, parent_options};
use crate::theme;

pub fn room_password_dialog(
    ctx: &egui::Context,
    dialog: &mut Option<RoomPasswordDialog>,
) -> Vec<UserAction> {
    let Some(current) = dialog.as_mut() else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    let frame = egui::Frame::window(&ctx.global_style())
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(18));
    let modal = egui::Modal::new(egui::Id::unique("room_password_dialog"))
        .backdrop_color(egui::Color32::from_black_alpha(180))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.set_max_width(400.0);
            ui.label(
                RichText::new("Sala com senha")
                    .font(theme::display(18.0))
                    .color(theme::TEXT),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "Digite a senha para entrar em \"{}\".",
                    current.title
                ))
                .color(theme::TEXT_SECONDARY),
            );
            ui.add_space(10.0);
            let input = ui.add_enabled(
                !current.busy,
                egui::TextEdit::singleline(&mut current.password.0)
                    .password(true)
                    .char_limit(MAX_ROOM_PASSWORD_LEN)
                    .desired_width(f32::INFINITY)
                    .hint_text("Senha da sala"),
            );
            let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !enter && ui.ctx().memory(|m| m.focused().is_none()) {
                input.request_focus();
            }
            if let Some(err) = &current.error {
                ui.add_space(6.0);
                ui.colored_label(theme::ERROR, err);
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let label = if current.busy {
                    "Entrando..."
                } else {
                    "Entrar"
                };
                let confirm = ui.add_enabled(!current.busy, theme::accent_button(label));
                if confirm.clicked() || enter {
                    actions.push(UserAction::SubmitRoomPassword);
                }
                if ui.button("Cancelar").clicked() {
                    actions.push(UserAction::CancelRoomPassword);
                }
            });
        });
    if modal.should_close() {
        actions.push(UserAction::CancelRoomPassword);
    }
    actions
}

pub fn admin_dialog(
    ctx: &egui::Context,
    dialog: &mut Option<AdminDialog>,
    rooms: &[RoomSummary],
) -> Vec<UserAction> {
    let Some(current) = dialog.as_mut() else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    let frame = egui::Frame::window(&ctx.global_style())
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(18));

    let modal = egui::Modal::new(egui::Id::unique("admin_dialog"))
        .backdrop_color(egui::Color32::from_black_alpha(180))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.set_max_width(400.0);
            match current {
                AdminDialog::CreateRoom {
                    title,
                    password,
                    parent,
                    error,
                } => {
                    room_edit_dialog_ui(
                        ui,
                        ("Nova sala", "Criar"),
                        RoomEditFields {
                            title,
                            password: &mut password.0,
                            remove_password: None,
                            parent,
                            parents: parent_options(rooms, None),
                        },
                        error.as_deref(),
                        &mut actions,
                    );
                }
                AdminDialog::EditRoom {
                    id,
                    title,
                    password,
                    remove_password,
                    locked,
                    parent,
                    error,
                    ..
                } => {
                    room_edit_dialog_ui(
                        ui,
                        ("Editar sala", "Salvar"),
                        RoomEditFields {
                            title,
                            password: &mut password.0,
                            remove_password: locked.then_some(remove_password),
                            parent,
                            parents: parent_options(rooms, Some(id)),
                        },
                        error.as_deref(),
                        &mut actions,
                    );
                }
                AdminDialog::DeleteRoom { title, .. } => {
                    confirm_danger_dialog_ui(
                        ui,
                        "Apagar sala",
                        &format!(
                            "Apagar a sala \"{title}\"? Todos os participantes serão removidos dela."
                        ),
                        "Apagar",
                        &mut actions,
                    );
                }
                AdminDialog::KickParticipant { name, .. } => {
                    confirm_danger_dialog_ui(
                        ui,
                        "Remover participante",
                        &format!("Remover \"{name}\" da sala atual?"),
                        "Remover",
                        &mut actions,
                    );
                }
            }
        });

    if modal.should_close() {
        actions.push(UserAction::CancelAdminDialog);
    }
    actions
}

pub fn update_confirm_dialog(ctx: &egui::Context, confirming: bool) -> Vec<UserAction> {
    if !confirming {
        return Vec::new();
    }
    let mut actions = Vec::new();
    let frame = egui::Frame::window(&ctx.global_style())
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(18));

    let modal = egui::Modal::new(egui::Id::unique("update_confirm_dialog"))
        .backdrop_color(egui::Color32::from_black_alpha(180))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.set_max_width(400.0);
            ui.label(
                RichText::new("Atualizar Zenha")
                    .font(theme::display(18.0))
                    .color(theme::TEXT),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new("Atualizar agora? Você vai sair da sala.")
                    .color(theme::TEXT_SECONDARY),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.add(theme::accent_button("Atualizar")).clicked() {
                    actions.push(UserAction::ConfirmUpdate);
                }
                if ui.button("Cancelar").clicked() {
                    actions.push(UserAction::CancelUpdateConfirm);
                }
            });
        });

    if modal.should_close() {
        actions.push(UserAction::CancelUpdateConfirm);
    }
    actions
}

struct RoomEditFields<'a> {
    title: &'a mut String,
    password: &'a mut String,
    /// `Some` quando a sala já tem senha e ela pode ser removida.
    remove_password: Option<&'a mut bool>,
    parent: &'a mut Option<String>,
    parents: Vec<&'a RoomSummary>,
}

fn room_edit_dialog_ui(
    ui: &mut egui::Ui,
    (heading, confirm_label): (&str, &str),
    fields: RoomEditFields<'_>,
    error: Option<&str>,
    actions: &mut Vec<UserAction>,
) {
    let RoomEditFields {
        title,
        password,
        remove_password,
        parent,
        parents,
    } = fields;
    ui.label(
        RichText::new(heading)
            .font(theme::display(18.0))
            .color(theme::TEXT),
    );
    ui.add_space(10.0);
    let input = ui.add(
        egui::TextEdit::singleline(title)
            .char_limit(MAX_ROOM_TITLE_LEN)
            .desired_width(f32::INFINITY)
            .hint_text("Nome da sala"),
    );
    let title_enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if !title_enter && ui.ctx().memory(|m| m.focused().is_none()) {
        input.request_focus();
    }
    ui.add_space(10.0);
    let has_password = remove_password.is_some();
    let removing = remove_password.as_deref().copied().unwrap_or(false);
    let hint = if has_password {
        "Nova senha (vazio mantém a atual)"
    } else {
        "Senha (vazio = sala pública)"
    };
    let password_input = ui.add_enabled(
        !removing,
        egui::TextEdit::singleline(password)
            .password(true)
            .char_limit(MAX_ROOM_PASSWORD_LEN)
            .desired_width(f32::INFINITY)
            .hint_text(hint),
    );
    if let Some(remove) = remove_password {
        ui.checkbox(remove, "Remover a senha");
    }
    if !parents.is_empty() || parent.is_some() {
        ui.add_space(10.0);
        let selected = parent.as_deref().map_or("Nenhuma", |id| {
            parents
                .iter()
                .find(|r| r.name == id)
                .map_or(id, |r| r.display_title())
        });
        egui::ComboBox::from_label("Sala-mãe")
            .selected_text(selected.to_owned())
            .show_ui(ui, |ui| {
                ui.selectable_value(parent, None, "Nenhuma");
                for room in &parents {
                    ui.selectable_value(parent, Some(room.name.clone()), room.display_title());
                }
            });
    }
    if let Some(err) = error {
        ui.add_space(6.0);
        ui.colored_label(theme::ERROR, err);
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let enter = title_enter
            || (password_input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
        if ui.add(theme::accent_button(confirm_label)).clicked() || enter {
            actions.push(UserAction::ConfirmAdminDialog);
        }
        if ui.button("Cancelar").clicked() {
            actions.push(UserAction::CancelAdminDialog);
        }
    });
}

fn confirm_danger_dialog_ui(
    ui: &mut egui::Ui,
    heading: &str,
    body: &str,
    confirm_label: &str,
    actions: &mut Vec<UserAction>,
) {
    ui.label(
        RichText::new(heading)
            .font(theme::display(18.0))
            .color(theme::TEXT),
    );
    ui.add_space(8.0);
    ui.label(RichText::new(body).color(theme::TEXT_SECONDARY));
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::Button::new(RichText::new(confirm_label).color(Color32::WHITE))
                    .fill(theme::ERROR),
            )
            .clicked()
        {
            actions.push(UserAction::ConfirmAdminDialog);
        }
        if ui.button("Cancelar").clicked() {
            actions.push(UserAction::CancelAdminDialog);
        }
    });
}

#[cfg(test)]
mod password_dialog_tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::state::SecretText;

    #[test]
    fn enter_in_the_room_password_field_submits() {
        let dialog = RoomPasswordDialog {
            room: "geral".to_owned(),
            title: "geral".to_owned(),
            password: SecretText(String::new()),
            error: None,
            busy: false,
        };
        let mut harness = Harness::builder().build_ui_state(
            |ui, state: &mut (Option<RoomPasswordDialog>, Vec<UserAction>)| {
                theme::install(ui.ctx());
                let actions = room_password_dialog(ui.ctx(), &mut state.0);
                state.1.extend(actions);
            },
            (Some(dialog), Vec::new()),
        );
        harness.run_steps(2);
        harness
            .get_by_role(egui::accesskit::Role::PasswordInput)
            .type_text("abc");
        harness.run_steps(2);
        harness.key_press(egui::Key::Enter);
        harness.run_steps(2);
        assert!(
            harness.state().1.contains(&UserAction::SubmitRoomPassword),
            "{:?}",
            harness.state().1
        );
    }
}
