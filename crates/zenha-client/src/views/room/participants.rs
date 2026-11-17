//! Grade de participantes e seus cartões.

use std::collections::BTreeMap;

use eframe::egui::{self, Align, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Vec2};
use zenha_protocol::RoomSummary;

use crate::views::participant_menu::{
    ParticipantMenuCtx, VolumeMenuScope, participant_volume_badge, participant_volume_menu,
};
use crate::icons;
use crate::state::{LocalShare, RoomState, UserAction, participant_volume_for};
use crate::theme;
use crate::ui_model::ParticipantView;

const TWO_COL_BREAKPOINT: f32 = 520.0;

pub(super) fn participants_grid(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    participant_volumes: &BTreeMap<String, u16>,
    is_admin: bool,
    actions: &mut Vec<UserAction>,
) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let avail = ui.available_width();
            let gap = 14.0;
            let cols = if avail < TWO_COL_BREAKPOINT { 1 } else { 2 };
            let card_width = if cols == 2 {
                ((avail - gap) * 0.5).max(180.0)
            } else {
                avail.max(180.0)
            };

            for pair in state.participants.chunks(cols) {
                ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for participant in pair {
                        let volume =
                            participant_volume_for(participant_volumes, &participant.identity);
                        ui.allocate_ui(Vec2::new(card_width, 84.0), |ui| {
                            participant_card(
                                ui,
                                state,
                                rooms,
                                participant,
                                volume,
                                card_width,
                                is_admin,
                                actions,
                            );
                        });
                    }
                });
                ui.add_space(gap);
            }
        });
}

#[expect(
    clippy::too_many_arguments,
    reason = "egui view takes room state, admin context and actions directly"
)]
fn participant_card(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    participant: &ParticipantView,
    volume: u16,
    width: f32,
    is_admin: bool,
    actions: &mut Vec<UserAction>,
) {
    let is_self = !state.local_identity.is_empty() && participant.identity == state.local_identity;
    let is_sharing_remote = state.share.is_sharing_remote(&participant.identity);
    let is_sharing_local = is_self && state.share.local == LocalShare::Active;
    let border_color = if participant.speaking {
        theme::SPEAKING
    } else {
        theme::PANEL
    };

    let card_response = egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(2.0, border_color))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            let inner_width = (width - 36.0).max(80.0);
            ui.set_min_width(inner_width);
            ui.set_max_width(inner_width);
            ui.allocate_ui_with_layout(
                Vec2::new(inner_width, 52.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    theme::avatar(
                        ui,
                        52.0,
                        &participant.identity,
                        &participant.name,
                        participant.avatar_version.as_deref(),
                        is_self,
                    );
                    ui.add_space(8.0);

                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), 52.0),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            if is_sharing_remote
                                && ui.add(theme::accent_button("Assistir")).clicked()
                            {
                                actions.push(UserAction::Watch(participant.identity.clone()));
                            }

                            let text_col_w = ui.available_width().max(40.0);
                            ui.allocate_ui_with_layout(
                                Vec2::new(text_col_w, 48.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_max_width(text_col_w);
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    ui.spacing_mut().interact_size.y = 0.0;
                                    participant_name_row(ui, participant, is_self, volume);
                                    participant_status_row(
                                        ui,
                                        participant,
                                        is_sharing_remote || is_sharing_local,
                                    );
                                },
                            );
                        },
                    );
                },
            );
        })
        .response;

    if !is_self {
        participant_volume_menu(
            &card_response,
            ParticipantMenuCtx {
                scope: VolumeMenuScope::Grid,
                identity: &participant.identity,
                current_volume: volume,
                is_admin,
                is_sharing: is_sharing_remote,
                current_room: &state.room,
                rooms,
            },
            actions,
        );
    }
}

fn participant_name_row(
    ui: &mut egui::Ui,
    participant: &ParticipantView,
    is_self: bool,
    volume: u16,
) {
    let badge = if is_self {
        Some(("(você)".to_owned(), 13.0))
    } else {
        participant_volume_badge(volume, is_self).map(|label| (label, 12.0))
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let max_name_w = if badge.is_some() {
            (ui.available_width() - 48.0).max(24.0)
        } else {
            ui.available_width().max(24.0)
        };
        ui.scope(|ui| {
            ui.set_max_width(max_name_w);
            ui.add(
                egui::Label::new(
                    RichText::new(&participant.name)
                        .font(theme::semibold(15.0))
                        .color(theme::TEXT),
                )
                .truncate(),
            );
        });
        if let Some((label, size)) = badge {
            ui.label(RichText::new(label).size(size).color(theme::TEXT_WEAK));
        }
    });
}

fn participant_status_row(ui: &mut egui::Ui, participant: &ParticipantView, is_sharing: bool) {
    if is_sharing {
        ui.add(
            egui::Label::new(
                RichText::new("Transmitindo")
                    .size(13.0)
                    .color(theme::TEXT_WEAK),
            )
            .truncate(),
        );
    } else if participant.speaking {
        ui.add(
            egui::Label::new(
                RichText::new("Falando")
                    .font(theme::semibold(13.0))
                    .color(theme::SPEAKING),
            )
            .truncate(),
        );
    } else if participant.muted {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            icons::mic(ui.painter(), icon_rect, theme::TEXT_WEAK, true);
            ui.add(
                egui::Label::new(
                    RichText::new("Microfone desligado")
                        .size(13.0)
                        .color(theme::TEXT_WEAK),
                )
                .truncate(),
            );
        });
    } else {
        ui.add(
            egui::Label::new(RichText::new("Ouvindo").size(13.0).color(theme::TEXT_WEAK))
                .truncate(),
        );
    }
}
