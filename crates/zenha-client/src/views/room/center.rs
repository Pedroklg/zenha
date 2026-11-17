//! Room center: header, bottom control bar and main area.

use std::collections::BTreeMap;

use eframe::egui::{self, Align, CornerRadius, Layout, Margin, RichText, Stroke, Vec2};
use zenha_protocol::RoomSummary;

use crate::state::{LocalShare, RoomState, UserAction, room_display_title};
use crate::theme;
use crate::views::room_watch::local_share_preview;
use crate::views::share::VideoRenderer;

use super::HEADER_HEIGHT;
use super::banners::{collapsed_chat_notice_banner, share_error_banner};
use super::participants::participants_grid;

const FOOTER_HEIGHT: f32 = 72.0;

pub(super) fn main_center(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    preview_renderer: &mut VideoRenderer,
    participant_volumes: &BTreeMap<String, u16>,
    is_admin: bool,
    actions: &mut Vec<UserAction>,
) {
    let title = room_display_title(rooms, &state.room);
    egui::Panel::top("room_header")
        .exact_size(HEADER_HEIGHT)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(24, 0)),
        )
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.allocate_ui_with_layout(rect.size(), Layout::right_to_left(Align::Center), |ui| {
                if chat_toggle_button(ui, state.chat.open, state.chat.unread) {
                    actions.push(UserAction::ToggleChat);
                }
                ui.add_space(8.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(title)
                                .font(theme::display(20.0))
                                .color(theme::TEXT),
                        )
                        .truncate(),
                    );
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(people_count_label(state.participants.len()))
                                .size(13.0)
                                .color(theme::TEXT_WEAK),
                        )
                        .truncate(),
                    );
                });
            });
            let full = ui.clip_rect();
            ui.painter().hline(
                full.x_range(),
                rect.bottom(),
                Stroke::new(1.0, theme::BORDER),
            );
        });

    egui::Panel::bottom("room_footer")
        .exact_size(FOOTER_HEIGHT)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(24, 0)),
        )
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let full = ui.clip_rect();
            ui.painter()
                .hline(full.x_range(), rect.top(), Stroke::new(1.0, theme::BORDER));
            ui.allocate_ui_with_layout(rect.size(), Layout::left_to_right(Align::Center), |ui| {
                match state.share.local {
                    LocalShare::Idle => {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Transmitir tela").font(theme::semibold(14.0)),
                                )
                                .min_size(Vec2::new(130.0, 40.0)),
                            )
                            .clicked()
                        {
                            actions.push(UserAction::OpenSharePicker);
                        }
                    }
                    LocalShare::Starting => {
                        ui.add_enabled(
                            false,
                            egui::Button::new("Iniciando transmissão...")
                                .min_size(Vec2::new(160.0, 40.0)),
                        );
                    }
                    LocalShare::Active => {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Parar transmissão").font(theme::semibold(14.0)),
                                )
                                .min_size(Vec2::new(140.0, 40.0)),
                            )
                            .clicked()
                        {
                            actions.push(UserAction::StopShare);
                        }
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let leave_btn = egui::Button::new(
                        RichText::new("Sair da sala")
                            .font(theme::semibold(14.0))
                            .color(theme::ERROR),
                    )
                    .fill(theme::ERROR_BG)
                    .stroke(Stroke::new(1.0, theme::ERROR_BORDER))
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(116.0, 40.0));
                    if ui.add(leave_btn).clicked() {
                        actions.push(UserAction::Leave);
                    }
                });
            });
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(Margin::same(24)),
        )
        .show(ui, |ui| {
            share_error_banner(ui, state.share.error.as_deref(), actions);
            collapsed_chat_notice_banner(ui, state, actions);
            local_share_preview(ui, &state.share, preview_renderer, false, actions);
            participants_grid(ui, state, rooms, participant_volumes, is_admin, actions);
        });
}

pub(crate) fn chat_toggle_button(ui: &mut egui::Ui, open: bool, unread: usize) -> bool {
    let border = if open { theme::ACCENT } else { theme::BORDER };
    let mut text = egui::text::LayoutJob::default();
    RichText::new("Chat")
        .font(theme::semibold(13.0))
        .color(theme::TEXT)
        .append_to(
            &mut text,
            ui.style(),
            egui::FontSelection::Default,
            Align::Center,
        );
    if unread > 0 {
        RichText::new(format!("  {unread}"))
            .font(theme::semibold(13.0))
            .color(theme::ACCENT)
            .append_to(
                &mut text,
                ui.style(),
                egui::FontSelection::Default,
                Align::Center,
            );
    }
    ui.add(egui::Button::new(text).stroke(Stroke::new(1.0, border)))
        .clicked()
}

pub(super) fn people_count_label(count: usize) -> String {
    if count == 1 {
        "1 pessoa na sala".to_owned()
    } else {
        format!("{count} pessoas na sala")
    }
}
