//! Área da sala durante visualização ou prévia de transmissão de tela.

use std::collections::BTreeMap;

use eframe::egui::{self, Align, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Vec2};
use rooms_protocol::RoomSummary;

use super::participant_menu::{
    ParticipantMenuCtx, VolumeMenuScope, participant_volume_badge, participant_volume_menu,
};
use super::room::{
    HEADER_HEIGHT, chat_toggle_button, collapsed_chat_notice_banner, share_error_banner,
};
use super::share::VideoRenderer;
use crate::icons;
use crate::state::{
    LocalShare, RoomState, ShareState, UserAction, WatchState, participant_volume_for,
};
use crate::theme;
use crate::ui_model::FeedHandle;

pub(super) struct ShareRenderers<'a> {
    pub(super) watch: &'a mut VideoRenderer,
    pub(super) preview: &'a mut VideoRenderer,
}

#[expect(
    clippy::too_many_arguments,
    reason = "egui view takes room state, admin context and actions directly"
)]
pub(super) fn watching_center(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    watch: &WatchState,
    renderers: ShareRenderers<'_>,
    stream_volume: u8,
    participant_volumes: &BTreeMap<String, u16>,
    is_admin: bool,
    actions: &mut Vec<UserAction>,
) {
    let presenter_name = state
        .participants
        .iter()
        .find(|p| p.identity == watch.identity)
        .map_or(watch.identity.as_str(), |p| p.name.as_str());

    egui::Panel::top("watching_header")
        .exact_size(HEADER_HEIGHT)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(20, 0)),
        )
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.allocate_ui_with_layout(rect.size(), Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::accent_button("Parar de assistir")).clicked() {
                    actions.push(UserAction::StopWatching);
                }
                if chat_toggle_button(ui, state.chat.open, state.chat.unread) {
                    actions.push(UserAction::ToggleChat);
                }
                if ui.button("Tela cheia").clicked() {
                    actions.push(UserAction::ToggleFullscreen);
                }
                ui.add_space(8.0);
                if ui.available_width() < 200.0 {
                    return;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    live_badge(ui);
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Tela de {presenter_name}"))
                                .font(theme::display(18.0))
                                .color(theme::TEXT),
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

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(Margin::same(16)),
        )
        .show(ui, |ui| {
            share_error_banner(ui, state.share.error.as_deref(), actions);
            collapsed_chat_notice_banner(ui, state, actions);
            stream_volume_box(ui, stream_volume, actions);
            ui.add_space(8.0);
            local_share_preview(ui, &state.share, renderers.preview, true, actions);
            compact_participants_strip(ui, state, rooms, participant_volumes, is_admin, actions);
            ui.add_space(10.0);

            egui::Frame::new()
                .fill(theme::VIDEO_BG)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(4))
                .show(ui, |ui| {
                    renderers.watch.show(ui, watch);
                });
        });
}

fn live_badge(ui: &mut egui::Ui) {
    egui::Frame::new()
        .fill(theme::LIVE_BG)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot_rect.center(), 4.0, theme::LIVE_DOT);
                ui.label(
                    RichText::new("AO VIVO")
                        .font(theme::semibold(11.0))
                        .color(theme::ERROR),
                );
            });
        });
}

fn stream_volume_box(ui: &mut egui::Ui, stream_volume: u8, actions: &mut Vec<UserAction>) {
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Som da transmissão")
                        .size(12.0)
                        .color(theme::TEXT_WEAK),
                );
                ui.spacing_mut().slider_width = 140.0;
                let mut volume = stream_volume;
                let slider = ui.add(egui::Slider::new(&mut volume, 0..=100).suffix("%"));
                if volume != stream_volume {
                    actions.push(UserAction::SetStreamVolume(volume));
                }
                if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                    actions.push(UserAction::SaveAudioSettings);
                }
            });
        });
}

fn compact_participants_strip(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    participant_volumes: &BTreeMap<String, u16>,
    is_admin: bool,
    actions: &mut Vec<UserAction>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
        for participant in &state.participants {
            let is_self =
                !state.local_identity.is_empty() && participant.identity == state.local_identity;
            let volume = participant_volume_for(participant_volumes, &participant.identity);
            let border = if participant.speaking {
                Stroke::new(1.5, theme::SPEAKING)
            } else {
                Stroke::new(1.0, theme::BORDER)
            };
            let text_color = if participant.speaking {
                theme::SPEAKING
            } else {
                theme::TEXT_SECONDARY
            };
            let chip_response = egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(border)
                .corner_radius(CornerRadius::same(18))
                .inner_margin(Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        theme::avatar(
                            ui,
                            24.0,
                            &participant.identity,
                            &participant.name,
                            participant.avatar_version.as_deref(),
                            is_self,
                        );
                        let mut label = participant.name.clone();
                        if let Some(badge) = participant_volume_badge(volume, is_self) {
                            label.push_str(" · ");
                            label.push_str(&badge);
                        }
                        ui.label(
                            RichText::new(label)
                                .font(theme::semibold(13.0))
                                .color(text_color),
                        );
                        if participant.muted {
                            let (icon_rect, response) =
                                ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                            icons::mic(ui.painter(), icon_rect, theme::TEXT_WEAK, true);
                            response.on_hover_text("Microfone desligado");
                        }
                    });
                })
                .response;
            if !is_self {
                participant_volume_menu(
                    &chip_response,
                    ParticipantMenuCtx {
                        scope: VolumeMenuScope::Strip,
                        identity: &participant.identity,
                        current_volume: volume,
                        is_admin,
                        is_sharing: state.share.is_sharing_remote(&participant.identity),
                        current_room: &state.room,
                        rooms,
                    },
                    actions,
                );
            }
        }
    });
}

pub(super) fn local_share_preview(
    ui: &mut egui::Ui,
    share: &ShareState,
    preview_renderer: &mut VideoRenderer,
    compact: bool,
    actions: &mut Vec<UserAction>,
) {
    let Some(preview) = share
        .preview
        .as_ref()
        .filter(|_| share.local == LocalShare::Active)
    else {
        return;
    };
    if share.preview_collapsed {
        collapsed_share_bar(ui, compact, actions);
        ui.add_space(if compact { 8.0 } else { 16.0 });
        return;
    }
    let box_size = if compact {
        Vec2::new(144.0, 81.0)
    } else {
        let max_w = (ui.available_width() - 28.0).clamp(160.0, 304.0);
        Vec2::new(max_w, (max_w * 9.0 / 16.0).round())
    };
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.5, theme::ACCENT))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            if compact {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    preview_box(
                        ui,
                        box_size,
                        preview.serial,
                        &preview.feed,
                        preview_renderer,
                    );
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Você está transmitindo")
                                .font(theme::semibold(13.5))
                                .color(theme::TEXT),
                        );
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if ui.button("Recolher").clicked() {
                                actions.push(UserAction::ToggleSharePreviewCollapsed);
                            }
                            if ui.button("Parar transmissão").clicked() {
                                actions.push(UserAction::StopShare);
                            }
                        });
                    });
                });
            } else {
                ui.set_width(box_size.x);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    live_badge(ui);
                    ui.label(
                        RichText::new("Você está transmitindo")
                            .font(theme::semibold(14.0))
                            .color(theme::TEXT),
                    );
                });
                ui.add_space(8.0);
                preview_box(
                    ui,
                    box_size,
                    preview.serial,
                    &preview.feed,
                    preview_renderer,
                );
                ui.add_space(8.0);
                if ui.button("Recolher").clicked() {
                    actions.push(UserAction::ToggleSharePreviewCollapsed);
                }
            }
        });
    ui.add_space(if compact { 8.0 } else { 16.0 });
}

fn collapsed_share_bar(ui: &mut egui::Ui, compact: bool, actions: &mut Vec<UserAction>) {
    let padding = if compact {
        Margin::symmetric(12, 8)
    } else {
        Margin::symmetric(12, 10)
    };
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.5, theme::ACCENT))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(padding)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                live_badge(ui);
                ui.label(
                    RichText::new("Você está transmitindo")
                        .font(theme::semibold(13.5))
                        .color(theme::TEXT),
                );
                if ui.button("Expandir").clicked() {
                    actions.push(UserAction::ToggleSharePreviewCollapsed);
                }
                if ui.button("Parar transmissão").clicked() {
                    actions.push(UserAction::StopShare);
                }
            });
        });
}

fn preview_box(
    ui: &mut egui::Ui,
    size: Vec2,
    serial: u64,
    feed: &FeedHandle,
    renderer: &mut VideoRenderer,
) {
    egui::Frame::new()
        .fill(theme::VIDEO_BG)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            renderer.show_feed(ui, serial, feed, "Iniciando prévia...");
        });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use eframe::egui;

    use super::*;
    use crate::ui_model::{FrameFeed, UiFrame};

    struct CountingFeed(AtomicUsize);

    impl FrameFeed for CountingFeed {
        fn take_newer(&self, _last_seen: &mut u64) -> Option<UiFrame> {
            self.0.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    #[test]
    fn collapsed_preview_stops_pulling_frames_from_feed() {
        let counter = Arc::new(CountingFeed(AtomicUsize::new(0)));
        let feed = FeedHandle(Arc::clone(&counter) as Arc<dyn FrameFeed>);
        let mut share = ShareState::with_test_preview(feed, true);
        let mut renderer = VideoRenderer::default();
        let ctx = egui::Context::default();
        let mut actions = Vec::new();

        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            local_share_preview(ui, &share, &mut renderer, false, &mut actions);
            local_share_preview(ui, &share, &mut renderer, true, &mut actions);
        });
        out.textures_delta.clear();
        assert_eq!(counter.0.load(Ordering::Relaxed), 0);

        share.preview_collapsed = false;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            local_share_preview(ui, &share, &mut renderer, false, &mut actions);
        });
        out.textures_delta.clear();
        assert_eq!(counter.0.load(Ordering::Relaxed), 1);
    }
}
