use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Vec2,
};
use rooms_protocol::RoomSummary;

use super::chat;
use super::share::VideoRenderer;
use crate::chat_images::ChatImages;
use crate::icons;
use crate::state::{ConnectionStatus, LocalShare, RoomState, UserAction, WatchState};
use crate::theme;
use crate::ui_model::ParticipantView;

const SIDEBAR_WIDTH: f32 = 256.0;
const HEADER_HEIGHT: f32 = 64.0;
const FOOTER_HEIGHT: f32 = 72.0;
const TWO_COL_BREAKPOINT: f32 = 520.0;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut RoomState,
    rooms: &[RoomSummary],
    server_host: &str,
    renderer: &mut VideoRenderer,
    images: &mut ChatImages,
    stream_volume: u8,
    ptt_hint: Option<&str>,
) -> Vec<UserAction> {
    let mut actions = Vec::new();

    if let Some(watch) = state.share.watching.as_ref().filter(|w| w.fullscreen) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::VIDEO_BG))
            .show(ui, |ui| {
                renderer.show(ui, watch);
                let button = egui::Area::new(egui::Id::unique("exit_fullscreen"))
                    .anchor(egui::Align2::RIGHT_TOP, [-12.0, 12.0])
                    .show(ui.ctx(), |ui| ui.button("Sair da tela cheia"));
                if button.inner.clicked() {
                    actions.push(UserAction::ToggleFullscreen);
                }
            });
        return actions;
    }

    sidebar(ui, state, rooms, server_host, ptt_hint, &mut actions);

    if state.chat.open {
        actions.extend(chat::show(
            ui,
            &mut state.chat,
            &state.participants,
            &state.local_identity,
            images,
        ));
    }

    if let Some(watch) = state.share.watching.clone() {
        watching_center(ui, state, &watch, renderer, stream_volume, &mut actions);
    } else {
        main_center(ui, state, &mut actions);
    }

    actions
}

fn sidebar(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    server_host: &str,
    ptt_hint: Option<&str>,
    actions: &mut Vec<UserAction>,
) {
    egui::Panel::left("rooms_panel")
        .exact_size(SIDEBAR_WIDTH)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::ZERO),
        )
        .show(ui, |ui| {
            egui::Panel::top("sidebar_brand")
                .exact_size(HEADER_HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::symmetric(16, 10)),
                )
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(
                            RichText::new("Zenha")
                                .font(theme::display(20.0))
                                .color(theme::TEXT),
                        );
                        if !server_host.is_empty() {
                            ui.label(
                                RichText::new(server_host)
                                    .size(12.0)
                                    .color(theme::TEXT_WEAK),
                            );
                        }
                    });
                    let full = ui.clip_rect();
                    let bottom = ui.max_rect().bottom() + 10.0;
                    ui.painter()
                        .hline(full.x_range(), bottom, Stroke::new(1.0, theme::BORDER));
                });

            egui::Panel::bottom("sidebar_user_footer")
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL_DARK)
                        .inner_margin(Margin::same(12)),
                )
                .show(ui, |ui| {
                    let full = ui.clip_rect();
                    let top = ui.max_rect().top() - 12.0;
                    ui.painter()
                        .hline(full.x_range(), top, Stroke::new(1.0, theme::BORDER));
                    user_footer(ui, state, ptt_hint, actions);
                });

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::same(12)),
                )
                .show(ui, |ui| {
                    rooms_list(ui, &state.room, &state.participants, rooms, actions);
                });
        });
}

fn user_footer(
    ui: &mut egui::Ui,
    state: &RoomState,
    ptt_hint: Option<&str>,
    actions: &mut Vec<UserAction>,
) {
    let local_name = local_display_name(&state.participants, &state.local_identity);
    let avatar_colors = theme::avatar_color(&state.local_identity, true);

    ui.horizontal(|ui| {
        theme::avatar(ui, 36.0, local_name, avatar_colors);
        ui.add_space(4.0);

        let text_width = (ui.available_width() - 96.0).max(40.0);
        ui.allocate_ui_with_layout(
            Vec2::new(text_width, 36.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(
                    egui::Label::new(
                        RichText::new(local_name)
                            .font(theme::semibold(13.0))
                            .color(theme::TEXT),
                    )
                    .truncate(),
                );
                if let Some(hint) = ptt_hint {
                    ui.add(
                        egui::Label::new(RichText::new(hint).size(11.0).color(theme::TEXT_WEAK))
                            .truncate(),
                    );
                } else {
                    let status_color = if state.status == ConnectionStatus::Connected {
                        theme::SPEAKING
                    } else {
                        theme::TEXT_WEAK
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(state.status.label())
                                .size(12.0)
                                .color(status_color),
                        )
                        .truncate(),
                    );
                }
            },
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let deafen_tip = if state.deafened {
                "Ligar áudio"
            } else {
                "Desligar áudio"
            };
            if icons::icon_button(ui, 44.0, icons::headphones, state.deafened, deafen_tip).clicked()
            {
                actions.push(UserAction::ToggleDeafen);
            }

            let mute_tip = if state.muted {
                "Ligar microfone"
            } else {
                "Mutar microfone"
            };
            let mute_resp = ui.add_enabled_ui(!state.deafened, |ui| {
                icons::icon_button(ui, 44.0, icons::mic, state.muted, mute_tip)
            });
            if mute_resp.inner.clicked() {
                actions.push(UserAction::ToggleMute);
            }
        });
    });
}

fn rooms_list(
    ui: &mut egui::Ui,
    current: &str,
    current_participants: &[ParticipantView],
    rooms: &[RoomSummary],
    actions: &mut Vec<UserAction>,
) {
    ui.label(
        RichText::new("SALAS")
            .font(theme::semibold(11.0))
            .color(theme::TEXT_WEAK),
    );
    ui.add_space(6.0);

    if rooms.is_empty() {
        ui.weak("Carregando salas...");
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for summary in rooms {
                let is_current = summary.name == current;
                let row_resp = room_row(ui, &summary.name, summary.participants.len(), is_current);
                if row_resp.clicked() && !is_current {
                    actions.push(UserAction::SwitchRoom(summary.name.clone()));
                }
                if !summary.participants.is_empty() {
                    ui.add_space(2.0);
                    for name in &summary.participants {
                        let speaking = is_occupant_speaking(is_current, name, current_participants);
                        occupant_row(ui, name, speaking);
                    }
                    ui.add_space(6.0);
                } else {
                    ui.add_space(4.0);
                }
            }
        });
}

fn room_row(ui: &mut egui::Ui, name: &str, count: usize, is_current: bool) -> egui::Response {
    let desired = Vec2::new(ui.available_width(), 44.0);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let bg = if is_current {
            theme::ACTIVE_ROOM_BG
        } else if response.hovered() {
            theme::CONTROL
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            painter.rect_filled(rect, CornerRadius::same(8), bg);
        }
        let (name_font, name_color) = if is_current {
            (theme::semibold(15.0), theme::ACCENT_TEXT)
        } else {
            (egui::FontId::proportional(15.0), theme::TEXT)
        };
        let count_color = if is_current {
            theme::ACCENT_TEXT
        } else {
            theme::TEXT_WEAK
        };
        let left_pos = egui::pos2(rect.left() + 12.0, rect.center().y);
        let right_pos = egui::pos2(rect.right() - 12.0, rect.center().y);
        painter.text(
            left_pos,
            egui::Align2::LEFT_CENTER,
            name,
            name_font,
            name_color,
        );
        painter.text(
            right_pos,
            egui::Align2::RIGHT_CENTER,
            count.to_string(),
            egui::FontId::proportional(12.0),
            count_color,
        );
    }
    response
}

fn occupant_row(ui: &mut egui::Ui, name: &str, speaking: bool) {
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
        let dot_color = if speaking {
            theme::SPEAKING
        } else {
            theme::INACTIVE_DOT
        };
        ui.painter()
            .circle_filled(dot_rect.center(), 4.0, dot_color);
        let text_color = if speaking {
            theme::TEXT
        } else {
            theme::TEXT_WEAK
        };
        ui.label(RichText::new(name).size(13.0).color(text_color));
    });
}

fn main_center(ui: &mut egui::Ui, state: &RoomState, actions: &mut Vec<UserAction>) {
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
                if ui.button("Configurações").clicked() {
                    actions.push(UserAction::OpenSettings);
                }
                if chat_toggle_button(ui, state.chat.open, state.chat.unread) {
                    actions.push(UserAction::ToggleChat);
                }
                ui.add_space(8.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&state.room)
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
            participants_grid(ui, state, actions);
        });
}

fn watching_center(
    ui: &mut egui::Ui,
    state: &RoomState,
    watch: &WatchState,
    renderer: &mut VideoRenderer,
    stream_volume: u8,
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
            stream_volume_box(ui, stream_volume, actions);
            ui.add_space(8.0);
            compact_participants_strip(ui, state);
            ui.add_space(10.0);

            egui::Frame::new()
                .fill(theme::VIDEO_BG)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(4))
                .show(ui, |ui| {
                    renderer.show(ui, watch);
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

fn compact_participants_strip(ui: &mut egui::Ui, state: &RoomState) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
        for participant in &state.participants {
            let is_self =
                !state.local_identity.is_empty() && participant.identity == state.local_identity;
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
            egui::Frame::new()
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
                            &participant.name,
                            theme::avatar_color(&participant.identity, is_self),
                        );
                        let label = if participant.muted {
                            format!("{} (mudo)", participant.name)
                        } else {
                            participant.name.clone()
                        };
                        ui.label(
                            RichText::new(label)
                                .font(theme::semibold(13.0))
                                .color(text_color),
                        );
                    });
                });
        }
    });
}

fn share_error_banner(ui: &mut egui::Ui, error: Option<&str>, actions: &mut Vec<UserAction>) {
    let Some(error) = error else {
        return;
    };
    egui::Frame::new()
        .fill(theme::ERROR_BG)
        .stroke(Stroke::new(1.0, theme::ERROR_BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(theme::ERROR, error);
                if ui.small_button("Fechar").clicked() {
                    actions.push(UserAction::DismissShareError);
                }
            });
        });
    ui.add_space(12.0);
}

fn participants_grid(ui: &mut egui::Ui, state: &RoomState, actions: &mut Vec<UserAction>) {
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
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for participant in pair {
                        ui.allocate_ui(Vec2::new(card_width, 84.0), |ui| {
                            participant_card(ui, state, participant, card_width, actions);
                        });
                    }
                });
                ui.add_space(gap);
            }
        });
}

fn participant_card(
    ui: &mut egui::Ui,
    state: &RoomState,
    participant: &ParticipantView,
    width: f32,
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

    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(2.0, border_color))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            let inner_width = (width - 36.0).max(80.0);
            ui.set_min_width(inner_width);
            ui.set_max_width(inner_width);
            ui.horizontal(|ui| {
                theme::avatar(
                    ui,
                    52.0,
                    &participant.name,
                    theme::avatar_color(&participant.identity, is_self),
                );
                ui.add_space(8.0);

                let text_col_w = if is_sharing_remote {
                    (ui.available_width() - 88.0).max(40.0)
                } else {
                    ui.available_width().max(40.0)
                };

                ui.allocate_ui_with_layout(
                    Vec2::new(text_col_w, 48.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(
                                RichText::new(&participant.name)
                                    .font(theme::semibold(15.0))
                                    .color(theme::TEXT),
                            );
                            if is_self {
                                ui.label(
                                    RichText::new("(você)").size(13.0).color(theme::TEXT_WEAK),
                                );
                            }
                        });

                        if is_sharing_remote || is_sharing_local {
                            ui.label(
                                RichText::new("Transmitindo a tela")
                                    .size(13.0)
                                    .color(theme::TEXT_WEAK),
                            );
                        } else if participant.speaking {
                            ui.label(
                                RichText::new("Falando")
                                    .font(theme::semibold(13.0))
                                    .color(theme::SPEAKING),
                            );
                        } else if participant.muted {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let (icon_rect, _) =
                                    ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                                icons::mic(ui.painter(), icon_rect, theme::TEXT_WEAK, true);
                                ui.label(
                                    RichText::new("Microfone desligado")
                                        .size(13.0)
                                        .color(theme::TEXT_WEAK),
                                );
                            });
                        } else {
                            ui.label(RichText::new("Ouvindo").size(13.0).color(theme::TEXT_WEAK));
                        }
                    },
                );

                if is_sharing_remote {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(theme::accent_button("Assistir")).clicked() {
                            actions.push(UserAction::Watch(participant.identity.clone()));
                        }
                    });
                }
            });
        });
}

fn chat_toggle_button(ui: &mut egui::Ui, open: bool, unread: usize) -> bool {
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

fn people_count_label(count: usize) -> String {
    if count == 1 {
        "1 pessoa na sala".to_owned()
    } else {
        format!("{count} pessoas na sala")
    }
}

fn local_display_name<'a>(participants: &'a [ParticipantView], local_identity: &'a str) -> &'a str {
    participants
        .iter()
        .find(|p| !local_identity.is_empty() && p.identity == local_identity)
        .map_or_else(
            || {
                if local_identity.is_empty() {
                    "Você"
                } else {
                    local_identity
                }
            },
            |p| p.name.as_str(),
        )
}

fn is_occupant_speaking(
    is_current_room: bool,
    name: &str,
    current_participants: &[ParticipantView],
) -> bool {
    is_current_room
        && current_participants
            .iter()
            .any(|p| p.name == name && p.speaking)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_people_count_in_portuguese() {
        assert_eq!(people_count_label(0), "0 pessoas na sala");
        assert_eq!(people_count_label(1), "1 pessoa na sala");
        assert_eq!(people_count_label(4), "4 pessoas na sala");
    }

    #[test]
    fn resolves_local_display_name_from_participants_or_fallback() {
        let list = vec![ParticipantView {
            identity: "id-1".to_owned(),
            name: "Lucas".to_owned(),
            muted: false,
            speaking: false,
        }];
        assert_eq!(local_display_name(&list, "id-1"), "Lucas");
        assert_eq!(local_display_name(&list, "id-2"), "id-2");
        assert_eq!(local_display_name(&list, ""), "Você");
    }

    #[test]
    fn occupant_speaking_is_true_only_in_current_room_when_speaking() {
        let list = vec![ParticipantView {
            identity: "id-ana".to_owned(),
            name: "Ana".to_owned(),
            muted: false,
            speaking: true,
        }];
        assert!(is_occupant_speaking(true, "Ana", &list));
        assert!(!is_occupant_speaking(false, "Ana", &list));
        assert!(!is_occupant_speaking(true, "Bruno", &list));
    }
}
