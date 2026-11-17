//! Dismissible room notices.

use eframe::egui::{self, Color32, CornerRadius, Margin, Stroke};

use crate::state::{RoomState, UserAction};
use crate::theme;

pub(crate) fn share_error_banner(
    ui: &mut egui::Ui,
    error: Option<&str>,
    actions: &mut Vec<UserAction>,
) {
    if let Some(error) = error {
        dismissible_banner(
            ui,
            error,
            theme::ERROR,
            theme::ERROR_BG,
            theme::ERROR_BORDER,
            || UserAction::DismissShareError,
            actions,
        );
    }
}

pub(crate) fn collapsed_chat_notice_banner(
    ui: &mut egui::Ui,
    state: &RoomState,
    actions: &mut Vec<UserAction>,
) {
    if state.chat.open {
        return;
    }
    if let Some(message) = state.chat.notice.as_deref() {
        dismissible_banner(
            ui,
            message,
            theme::TEXT,
            theme::PANEL_DARK,
            theme::BORDER,
            || UserAction::DismissChatNotice,
            actions,
        );
    }
}

pub(super) fn dismissible_banner(
    ui: &mut egui::Ui,
    message: &str,
    text_color: Color32,
    fill: Color32,
    border: Color32,
    dismiss_action: impl FnOnce() -> UserAction,
    actions: &mut Vec<UserAction>,
) {
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(text_color, message);
                if ui.small_button("Fechar").clicked() {
                    actions.push(dismiss_action());
                }
            });
        });
    ui.add_space(12.0);
}
