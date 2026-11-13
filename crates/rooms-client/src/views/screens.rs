use eframe::egui;

use crate::state::UserAction;
use crate::theme;

pub fn connecting(ui: &mut egui::Ui, room: &str) -> Vec<UserAction> {
    let mut actions = Vec::new();
    egui::CentralPanel::default()
        .frame(theme::central_frame())
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.spinner();
                ui.add_space(12.0);
                ui.label(format!("Conectando à sala \"{room}\"..."));
                ui.add_space(12.0);
                if ui.button("Cancelar").clicked() {
                    actions.push(UserAction::CancelConnect);
                }
            });
        });
    actions
}

pub fn demo_banner(ui: &mut egui::Ui, text: &str) {
    egui::Panel::top("demo_banner").show(ui, |ui| {
        ui.colored_label(ui.visuals().warn_fg_color, text);
    });
}
