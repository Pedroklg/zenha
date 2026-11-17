//! Admin section of the settings.

use super::*;

pub(super) fn admin_section(
    ui: &mut egui::Ui,
    can_admin: bool,
    admin: &mut AdminState,
    actions: &mut Vec<UserAction>,
) {
    section_heading(ui, "Administração", true);

    if !can_admin {
        ui.weak("Entre em um servidor para usar as ferramentas de administrador.");
        return;
    }

    if admin.active {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = ROW_GAP;
            ui.label(RichText::new("Admin ativo").color(theme::ACCENT_TEXT));
            if ui.button("Sair do admin").clicked() {
                actions.push(UserAction::AdminLogout);
            }
        });
        ui.add_space(ROW_GAP);
        ui.label("Novo código de convite");
        ui.add_space(CONTROL_GAP);
        let invite_resp = ui.add(
            egui::TextEdit::singleline(&mut admin.new_invite_input)
                .desired_width(INVITE_INPUT_WIDTH)
                .hint_text("Mínimo 8 caracteres"),
        );
        ui.add_space(CONTROL_GAP);
        ui.horizontal(|ui| {
            if ui.button("Trocar convite").clicked()
                || (invite_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                actions.push(UserAction::RotateInvite);
            }
        });
    } else {
        ui.label("Senha de administrador");
        ui.add_space(CONTROL_GAP);
        let pw_resp = ui.add_enabled(
            !admin.logging_in,
            egui::TextEdit::singleline(&mut admin.password_input)
                .password(true)
                .desired_width(COMBO_WIDTH),
        );
        ui.add_space(ROW_GAP);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = ROW_GAP;
            let enter = pw_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui
                .add_enabled(!admin.logging_in, egui::Button::new("Entrar como admin"))
                .clicked()
                || (!admin.logging_in && enter)
            {
                actions.push(UserAction::AdminLogin);
            }
            if admin.logging_in {
                ui.spinner();
            }
        });
    }

    let message = match (&admin.error, &admin.notice) {
        (Some(error), _) => Some((theme::ERROR, error)),
        (None, Some(notice)) => Some((theme::ACCENT_TEXT, notice)),
        (None, None) => None,
    };
    if let Some((color, text)) = message {
        ui.add_space(ROW_GAP);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = ROW_GAP;
            ui.colored_label(color, text);
            if ui.small_button("Fechar").clicked() {
                actions.push(UserAction::DismissAdminNotice);
            }
        });
    }
}
