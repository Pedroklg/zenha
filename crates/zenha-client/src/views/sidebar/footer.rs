//! Sidebar footer: update notice and local user.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Vec2,
};

use super::SidebarUser;
use crate::format::format_size;
use crate::icons;
use crate::state::{ConnectionStatus, UpdateBanner, UpdatePhase, UserAction};
use crate::theme;

const FOOTER_AVATAR_SIZE: f32 = 32.0;
const FOOTER_BUTTON_SIZE: f32 = 32.0;
const FOOTER_BUTTON_GAP: f32 = 4.0;

pub(super) fn update_footer(
    ui: &mut egui::Ui,
    banner: &UpdateBanner,
    actions: &mut Vec<UserAction>,
) {
    let failed = matches!(banner.phase, UpdatePhase::Failed(_));
    let (fill, border) = if failed {
        (theme::ERROR_BG, theme::ERROR_BORDER)
    } else {
        (theme::CONTROL, theme::BORDER)
    };

    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let show_dismiss = !matches!(banner.phase, UpdatePhase::Downloading { .. });
            ui.horizontal(|ui| {
                let link_text = RichText::new(format!("Nova versão {}", banner.version))
                    .size(12.0)
                    .color(theme::ACCENT_TEXT);
                if ui
                    .add(egui::Link::new(link_text))
                    .on_hover_text("Abrir página do release")
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&banner.url));
                }

                if show_dismiss {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close = egui::Button::new(
                            RichText::new("×").size(12.0).color(theme::TEXT_WEAK),
                        )
                        .frame(false);
                        if ui.add(close).on_hover_text("Dispensar").clicked() {
                            actions.push(UserAction::DismissUpdate);
                        }
                    });
                }
            });

            if !banner.can_install {
                return;
            }

            match &banner.phase {
                UpdatePhase::Available => {
                    ui.add_space(6.0);
                    let btn = egui::Button::new(
                        RichText::new("Atualizar")
                            .font(theme::semibold(12.0))
                            .color(theme::ON_ACCENT),
                    )
                    .fill(theme::ACCENT)
                    .stroke(Stroke::NONE)
                    .corner_radius(CornerRadius::same(6));
                    if ui.add(btn).clicked() {
                        actions.push(UserAction::StartUpdate);
                    }
                }
                UpdatePhase::Confirming => {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Confirmando…")
                            .size(12.0)
                            .color(theme::TEXT_WEAK),
                    );
                }
                UpdatePhase::Downloading { downloaded, total } => {
                    let fraction = if *total == 0 {
                        0.0
                    } else {
                        (*downloaded as f32 / *total as f32).clamp(0.0, 1.0)
                    };
                    ui.add_space(6.0);
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_height(8.0)
                            .fill(theme::ACCENT),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let pct = (fraction * 100.0).round();
                        let status = if *total > 0 {
                            format!("{pct:.0}% · {}", format_size(*downloaded))
                        } else {
                            "Baixando…".to_owned()
                        };
                        ui.label(RichText::new(status).size(11.0).color(theme::TEXT_WEAK));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Cancelar").clicked() {
                                actions.push(UserAction::CancelUpdateDownload);
                            }
                        });
                    });
                }
                UpdatePhase::Ready(_) => {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Pronto para instalar")
                            .size(12.0)
                            .color(theme::ACCENT_TEXT),
                    );
                }
                UpdatePhase::Failed(message) => {
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(RichText::new(message).size(12.0).color(theme::ERROR))
                            .wrap(),
                    );
                    ui.add_space(6.0);
                    if ui.small_button("Tentar de novo").clicked() {
                        actions.push(UserAction::RetryUpdate);
                    }
                }
            }
        });
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FooterStatus {
    color: Color32,
    tooltip: String,
}

fn footer_status(connection: Option<ConnectionStatus>, ptt_hint: Option<&str>) -> FooterStatus {
    match connection {
        None => FooterStatus {
            color: theme::INACTIVE_DOT,
            tooltip: "Fora de sala".to_owned(),
        },
        Some(ConnectionStatus::Connected) => {
            let tooltip = match ptt_hint {
                Some(hint) => format!("{} · {hint}", ConnectionStatus::Connected.label()),
                None => ConnectionStatus::Connected.label().to_owned(),
            };
            FooterStatus {
                color: theme::ACCENT,
                tooltip,
            }
        }
        Some(status @ (ConnectionStatus::Connecting | ConnectionStatus::Reconnecting)) => {
            FooterStatus {
                color: theme::STATUS_PENDING,
                tooltip: status.label().to_owned(),
            }
        }
    }
}

pub(super) fn user_footer(ui: &mut egui::Ui, user: SidebarUser<'_>, actions: &mut Vec<UserAction>) {
    let status = footer_status(user.connection, user.ptt_hint);
    let row_width = ui.available_width();

    ui.allocate_ui_with_layout(
        Vec2::new(row_width, FOOTER_AVATAR_SIZE),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            theme::avatar(
                ui,
                FOOTER_AVATAR_SIZE,
                user.identity,
                user.name,
                user.avatar_version,
                true,
            );

            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), FOOTER_AVATAR_SIZE),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = FOOTER_BUTTON_GAP;
                    if icons::icon_button(
                        ui,
                        FOOTER_BUTTON_SIZE,
                        icons::gear,
                        false,
                        "Configurações",
                    )
                    .clicked()
                    {
                        actions.push(UserAction::OpenSettings);
                    }

                    let deafen_tip = if user.deafened {
                        "Ligar áudio"
                    } else {
                        "Desligar áudio"
                    };
                    if icons::icon_button(
                        ui,
                        FOOTER_BUTTON_SIZE,
                        icons::headphones,
                        user.deafened,
                        deafen_tip,
                    )
                    .clicked()
                    {
                        actions.push(UserAction::ToggleDeafen);
                    }

                    let mute_tip = if user.muted {
                        "Ligar microfone"
                    } else {
                        "Desligar microfone"
                    };
                    let mute_resp = ui.add_enabled_ui(!user.deafened, |ui| {
                        icons::icon_button(ui, FOOTER_BUTTON_SIZE, icons::mic, user.muted, mute_tip)
                    });
                    if mute_resp.inner.clicked() {
                        actions.push(UserAction::ToggleMute);
                    }

                    let identity_width = ui.available_width().max(24.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(identity_width, FOOTER_AVATAR_SIZE),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_max_width(identity_width);
                            ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                            ui.spacing_mut().item_spacing.x = 5.0;
                            let (dot_rect, dot_resp) =
                                ui.allocate_exact_size(Vec2::new(10.0, 20.0), Sense::hover());
                            if ui.is_rect_visible(dot_rect) {
                                ui.painter()
                                    .circle_filled(dot_rect.center(), 4.0, status.color);
                            }
                            dot_resp.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::Role::Label,
                                    true,
                                    status.tooltip.clone(),
                                )
                            });
                            dot_resp.on_hover_text(status.tooltip);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(user.name)
                                        .font(theme::semibold(13.0))
                                        .color(theme::TEXT),
                                )
                                .truncate(),
                            );
                        },
                    );
                },
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_status_maps_connection_states_to_dot_color_and_tooltip() {
        assert_eq!(
            footer_status(None, None),
            FooterStatus {
                color: theme::INACTIVE_DOT,
                tooltip: "Fora de sala".to_owned(),
            }
        );
        assert_eq!(
            footer_status(Some(ConnectionStatus::Connected), None),
            FooterStatus {
                color: theme::ACCENT,
                tooltip: "Conectado".to_owned(),
            }
        );
        assert_eq!(
            footer_status(
                Some(ConnectionStatus::Connected),
                Some("Segure F9 para falar"),
            ),
            FooterStatus {
                color: theme::ACCENT,
                tooltip: "Conectado · Segure F9 para falar".to_owned(),
            }
        );
        assert_eq!(
            footer_status(Some(ConnectionStatus::Connecting), None),
            FooterStatus {
                color: theme::STATUS_PENDING,
                tooltip: "Conectando…".to_owned(),
            }
        );
        assert_eq!(
            footer_status(Some(ConnectionStatus::Reconnecting), Some("ignored")),
            FooterStatus {
                color: theme::STATUS_PENDING,
                tooltip: "Reconectando…".to_owned(),
            }
        );
    }
}
