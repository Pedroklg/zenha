use eframe::egui;

use crate::state::{LoginForm, UserAction};

pub fn show(ui: &mut egui::Ui, form: &mut LoginForm) -> Vec<UserAction> {
    let mut actions = Vec::new();
    let errors = form.errors();
    let error_color = ui.visuals().error_fg_color;

    egui::CentralPanel::default().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.heading("Zenha");
            ui.add_space(16.0);
            ui.scope(|ui| {
                ui.set_max_width(380.0);
                field(
                    ui,
                    "Seu nome",
                    &mut form.display_name,
                    false,
                    "ex.: Pedro",
                    errors.display_name.as_deref(),
                    error_color,
                );
                field(
                    ui,
                    "Endereço do servidor",
                    &mut form.server_url,
                    false,
                    "http://127.0.0.1:8787",
                    errors.server_url.as_deref(),
                    error_color,
                );
                field(
                    ui,
                    "Código de convite",
                    &mut form.invite_code,
                    true,
                    "",
                    errors.invite_code.as_deref(),
                    error_color,
                );
                let room = field(
                    ui,
                    "Sala",
                    &mut form.room,
                    false,
                    "geral",
                    errors.room.as_deref(),
                    error_color,
                );
                ui.add_space(4.0);
                ui.checkbox(&mut form.muted_on_join, "Entrar com o microfone mudo");
                ui.add_space(12.0);
                let valid = errors.is_empty();
                let enter = ui
                    .add_enabled(
                        valid,
                        egui::Button::new("Entrar").min_size(egui::vec2(120.0, 28.0)),
                    )
                    .clicked();
                let enter_key = room.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if valid && (enter || enter_key) {
                    actions.push(UserAction::SubmitLogin);
                }
                ui.add_space(8.0);
                if ui.button("Configurações").clicked() {
                    actions.push(UserAction::OpenSettings);
                }
            });
        });
    });
    actions
}

fn field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    password: bool,
    hint: &str,
    error: Option<&str>,
    error_color: egui::Color32,
) -> egui::Response {
    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.label(label);
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .password(password)
                .hint_text(hint)
                .desired_width(f32::INFINITY),
        );
        if let Some(message) = error.filter(|_| !value.trim().is_empty()) {
            ui.colored_label(error_color, message);
        }
        ui.add_space(6.0);
        response
    })
    .inner
}
