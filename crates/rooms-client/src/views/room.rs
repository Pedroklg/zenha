use std::collections::BTreeMap;

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Vec2,
};
use rooms_protocol::RoomSummary;

use super::chat;
use super::share::VideoRenderer;
use crate::chat_images::ChatImages;
use crate::icons;
use crate::state::{
    ConnectionStatus, DEFAULT_PARTICIPANT_VOLUME, LobbyVoiceState, LocalShare,
    MAX_PARTICIPANT_VOLUME, RoomState, ShareState, UserAction, WatchState, participant_volume_for,
};
use crate::theme;
use crate::ui_model::{FeedHandle, ParticipantView};
use crate::update::AvailableUpdate;

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
    preview_renderer: &mut VideoRenderer,
    images: &mut ChatImages,
    stream_volume: u8,
    participant_volumes: &BTreeMap<String, u16>,
    ptt_hint: Option<&str>,
    available_update: Option<&AvailableUpdate>,
) -> Vec<UserAction> {
    let mut actions = Vec::new();
    if state.share.preview.is_none() {
        preview_renderer.reset();
    }

    if let Some(watch) = state.share.watching.as_ref().filter(|w| w.fullscreen) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::VIDEO_BG))
            .show(ui, |ui| {
                renderer.show(ui, watch);
                let button = egui::Area::new(egui::Id::unique("exit_fullscreen"))
                    .anchor(egui::Align2::RIGHT_TOP, [-12.0, 12.0])
                    .show(ui.ctx(), |ui| ui.button("Sair da tela cheia (Esc)"));
                if button.inner.clicked() {
                    actions.push(UserAction::ToggleFullscreen);
                }
            });
        return actions;
    }

    let local_name = local_display_name(&state.participants, &state.local_identity);
    let user = SidebarUser {
        name: local_name,
        status_label: ptt_hint.unwrap_or_else(|| state.status.label()),
        status_connected: ptt_hint.is_none() && state.status == ConnectionStatus::Connected,
        muted: state.muted,
        deafened: state.deafened,
    };
    sidebar(
        ui,
        SidebarArgs {
            user,
            current_room: Some(&state.room),
            local_identity: &state.local_identity,
            participants: &state.participants,
            participant_volumes,
            rooms,
            server_host,
            available_update,
        },
        &mut actions,
    );

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
        watching_center(
            ui,
            state,
            &watch,
            ShareRenderers {
                watch: renderer,
                preview: preview_renderer,
            },
            stream_volume,
            participant_volumes,
            &mut actions,
        );
    } else {
        main_center(
            ui,
            state,
            preview_renderer,
            participant_volumes,
            &mut actions,
        );
    }

    actions
}

pub fn show_lobby(
    ui: &mut egui::Ui,
    display_name: &str,
    voice: LobbyVoiceState,
    rooms: &[RoomSummary],
    server_host: &str,
    error: Option<&str>,
    available_update: Option<&AvailableUpdate>,
) -> Vec<UserAction> {
    let mut actions = Vec::new();
    let user = SidebarUser {
        name: display_name.trim(),
        status_label: "Fora de sala",
        status_connected: false,
        muted: voice.muted,
        deafened: voice.deafened,
    };
    let empty_volumes = BTreeMap::new();
    sidebar(
        ui,
        SidebarArgs {
            user,
            current_room: None,
            local_identity: "",
            participants: &[],
            participant_volumes: &empty_volumes,
            rooms,
            server_host,
            available_update,
        },
        &mut actions,
    );

    egui::Panel::top("lobby_header")
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
                .inner_margin(Margin::same(24)),
        )
        .show(ui, |ui| {
            if let Some(message) = error {
                egui::Frame::new()
                    .fill(theme::ERROR_BG)
                    .stroke(Stroke::new(1.0, theme::ERROR_BORDER))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.label(RichText::new(message).size(13.0).color(theme::ERROR));
                    });
                ui.add_space(16.0);
            }
            ui.allocate_ui_with_layout(
                ui.available_size(),
                Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    ui.label(
                        RichText::new("Escolha uma sala para entrar.")
                            .size(15.0)
                            .color(theme::TEXT_SECONDARY),
                    );
                },
            );
        });

    actions
}

struct SidebarUser<'a> {
    name: &'a str,
    status_label: &'a str,
    status_connected: bool,
    muted: bool,
    deafened: bool,
}

struct SidebarArgs<'a> {
    user: SidebarUser<'a>,
    current_room: Option<&'a str>,
    local_identity: &'a str,
    participants: &'a [ParticipantView],
    participant_volumes: &'a BTreeMap<String, u16>,
    rooms: &'a [RoomSummary],
    server_host: &'a str,
    available_update: Option<&'a AvailableUpdate>,
}

fn sidebar(ui: &mut egui::Ui, args: SidebarArgs<'_>, actions: &mut Vec<UserAction>) {
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
                        if !args.server_host.is_empty() {
                            ui.label(
                                RichText::new(args.server_host)
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
                    user_footer(ui, args.user, actions);
                    if let Some(update) = args.available_update {
                        ui.add_space(8.0);
                        update_footer(ui, update, actions);
                    }
                });

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::same(12)),
                )
                .show(ui, |ui| {
                    rooms_list(
                        ui,
                        RoomsListView {
                            current_room: args.current_room,
                            local_identity: args.local_identity,
                            participants: args.participants,
                            participant_volumes: args.participant_volumes,
                            rooms: args.rooms,
                        },
                        actions,
                    );
                });
        });
}

fn update_footer(ui: &mut egui::Ui, update: &AvailableUpdate, actions: &mut Vec<UserAction>) {
    egui::Frame::new()
        .fill(theme::CONTROL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let link_text = RichText::new(format!("Nova versão {}", update.version))
                    .size(12.0)
                    .color(theme::ACCENT_TEXT);
                if ui
                    .add(egui::Link::new(link_text))
                    .on_hover_text("Abrir página do release")
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&update.url));
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let close =
                        egui::Button::new(RichText::new("×").size(12.0).color(theme::TEXT_WEAK))
                            .frame(false);
                    if ui.add(close).on_hover_text("Dispensar").clicked() {
                        actions.push(UserAction::DismissUpdate);
                    }
                });
            });
        });
}

fn user_footer(ui: &mut egui::Ui, user: SidebarUser<'_>, actions: &mut Vec<UserAction>) {
    let avatar_colors = theme::avatar_color(user.name, true);

    ui.horizontal(|ui| {
        theme::avatar(ui, 36.0, user.name, avatar_colors);
        ui.add_space(4.0);

        let text_width = (ui.available_width() - 96.0).max(40.0);
        ui.allocate_ui_with_layout(
            Vec2::new(text_width, 36.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(
                    egui::Label::new(
                        RichText::new(user.name)
                            .font(theme::semibold(13.0))
                            .color(theme::TEXT),
                    )
                    .truncate(),
                );
                let status_color = if user.status_connected {
                    theme::SPEAKING
                } else {
                    theme::TEXT_WEAK
                };
                ui.add(
                    egui::Label::new(
                        RichText::new(user.status_label)
                            .size(12.0)
                            .color(status_color),
                    )
                    .truncate(),
                );
            },
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let deafen_tip = if user.deafened {
                "Ligar áudio"
            } else {
                "Desligar áudio"
            };
            if icons::icon_button(ui, 44.0, icons::headphones, user.deafened, deafen_tip).clicked()
            {
                actions.push(UserAction::ToggleDeafen);
            }

            let mute_tip = if user.muted {
                "Ligar microfone"
            } else {
                "Mutar microfone"
            };
            let mute_resp = ui.add_enabled_ui(!user.deafened, |ui| {
                icons::icon_button(ui, 44.0, icons::mic, user.muted, mute_tip)
            });
            if mute_resp.inner.clicked() {
                actions.push(UserAction::ToggleMute);
            }
        });
    });
}

struct RoomsListView<'a> {
    current_room: Option<&'a str>,
    local_identity: &'a str,
    participants: &'a [ParticipantView],
    participant_volumes: &'a BTreeMap<String, u16>,
    rooms: &'a [RoomSummary],
}

fn rooms_list(ui: &mut egui::Ui, view: RoomsListView<'_>, actions: &mut Vec<UserAction>) {
    ui.label(
        RichText::new("SALAS")
            .font(theme::semibold(11.0))
            .color(theme::TEXT_WEAK),
    );
    ui.add_space(6.0);

    if view.rooms.is_empty() {
        ui.weak("Carregando salas...");
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for summary in view.rooms {
                let is_current = view.current_room == Some(summary.name.as_str());
                let row_resp = room_row(ui, &summary.name, summary.participants.len(), is_current);
                if row_resp.clicked() && !is_current {
                    let action = if view.current_room.is_some() {
                        UserAction::SwitchRoom(summary.name.clone())
                    } else {
                        UserAction::JoinRoom(summary.name.clone())
                    };
                    actions.push(action);
                }
                if !summary.participants.is_empty() {
                    ui.add_space(2.0);
                    for (idx, name) in summary.participants.iter().enumerate() {
                        let speaking = is_occupant_speaking(is_current, name, view.participants);
                        let volume_target = occupant_volume_target(
                            is_current,
                            name,
                            &summary.participants,
                            view.participants,
                            view.local_identity,
                        )
                        .map(|participant| {
                            (
                                participant,
                                participant_volume_for(
                                    view.participant_volumes,
                                    &participant.identity,
                                ),
                            )
                        });
                        let row_id = volume_target.map_or_else(
                            || egui::IdSalt::new(("occupant_row_anon", &summary.name, idx, name)),
                            |(p, _)| {
                                egui::IdSalt::new((
                                    "occupant_row",
                                    &summary.name,
                                    p.identity.as_str(),
                                ))
                            },
                        );
                        ui.push_id(row_id, |ui| {
                            occupant_row(ui, name, speaking, volume_target, actions);
                        });
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

fn occupant_row(
    ui: &mut egui::Ui,
    name: &str,
    speaking: bool,
    volume_target: Option<(&ParticipantView, u16)>,
    actions: &mut Vec<UserAction>,
) {
    let interactive = volume_target.is_some();
    let sense = if interactive {
        Sense::click()
    } else {
        Sense::hover()
    };
    let row_response = ui
        .scope_builder(
            egui::UiBuilder::new()
                .layout(Layout::left_to_right(Align::Center))
                .sense(sense),
            |ui| {
                ui.set_min_width(ui.available_width());
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
                let volume_badge =
                    volume_target.and_then(|(_, volume)| participant_volume_badge(volume, false));
                let max_name_width = if volume_badge.is_some() {
                    (ui.available_width() - 44.0).max(24.0)
                } else {
                    ui.available_width().max(24.0)
                };
                ui.scope(|ui| {
                    ui.set_max_width(max_name_width);
                    let mut label =
                        egui::Label::new(RichText::new(name).size(13.0).color(text_color))
                            .truncate();
                    if interactive {
                        label = label.selectable(false);
                    }
                    ui.add(label);
                });
                if let Some(badge) = volume_badge {
                    ui.label(RichText::new(badge).size(11.0).color(theme::TEXT_WEAK));
                }
            },
        )
        .response;

    if let Some((participant, volume)) = volume_target {
        participant_volume_menu(
            &row_response,
            VolumeMenuScope::Sidebar,
            &participant.identity,
            volume,
            actions,
        );
    }
}

fn main_center(
    ui: &mut egui::Ui,
    state: &RoomState,
    preview_renderer: &mut VideoRenderer,
    participant_volumes: &BTreeMap<String, u16>,
    actions: &mut Vec<UserAction>,
) {
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
            local_share_preview(ui, &state.share, preview_renderer, false, actions);
            participants_grid(ui, state, participant_volumes, actions);
        });
}

struct ShareRenderers<'a> {
    watch: &'a mut VideoRenderer,
    preview: &'a mut VideoRenderer,
}

fn watching_center(
    ui: &mut egui::Ui,
    state: &RoomState,
    watch: &WatchState,
    renderers: ShareRenderers<'_>,
    stream_volume: u8,
    participant_volumes: &BTreeMap<String, u16>,
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
            local_share_preview(ui, &state.share, renderers.preview, true, actions);
            compact_participants_strip(ui, state, participant_volumes, actions);
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
    participant_volumes: &BTreeMap<String, u16>,
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
                            &participant.name,
                            theme::avatar_color(&participant.identity, is_self),
                        );
                        let mut label = if participant.muted {
                            format!("{} (mudo)", participant.name)
                        } else {
                            participant.name.clone()
                        };
                        if let Some(badge) = participant_volume_badge(volume, is_self) {
                            label.push_str(" · ");
                            label.push_str(&badge);
                        }
                        ui.label(
                            RichText::new(label)
                                .font(theme::semibold(13.0))
                                .color(text_color),
                        );
                    });
                })
                .response;
            if !is_self {
                participant_volume_menu(
                    &chip_response,
                    VolumeMenuScope::Strip,
                    &participant.identity,
                    volume,
                    actions,
                );
            }
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

fn local_share_preview(
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
    let box_size = if compact {
        Vec2::new(144.0, 81.0)
    } else {
        let max_w = ui.available_width().clamp(160.0, 280.0);
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
                        if ui.button("Parar transmissão").clicked() {
                            actions.push(UserAction::StopShare);
                        }
                    });
                });
            } else {
                ui.horizontal(|ui| {
                    live_badge(ui);
                    ui.add_space(6.0);
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
            }
        });
    ui.add_space(if compact { 8.0 } else { 16.0 });
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

fn participants_grid(
    ui: &mut egui::Ui,
    state: &RoomState,
    participant_volumes: &BTreeMap<String, u16>,
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
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for participant in pair {
                        let volume =
                            participant_volume_for(participant_volumes, &participant.identity);
                        ui.allocate_ui(Vec2::new(card_width, 84.0), |ui| {
                            participant_card(ui, state, participant, volume, card_width, actions);
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
    volume: u16,
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

    let card_response = egui::Frame::new()
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
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(
                                RichText::new(&participant.name)
                                    .font(theme::semibold(15.0))
                                    .color(theme::TEXT),
                            );
                            if is_self {
                                ui.label(
                                    RichText::new("(você)").size(13.0).color(theme::TEXT_WEAK),
                                );
                            } else if let Some(badge) = participant_volume_badge(volume, is_self) {
                                ui.label(RichText::new(badge).size(12.0).color(theme::TEXT_WEAK));
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
        })
        .response;

    if !is_self {
        participant_volume_menu(
            &card_response,
            VolumeMenuScope::Grid,
            &participant.identity,
            volume,
            actions,
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum VolumeMenuScope {
    Sidebar,
    Grid,
    Strip,
}

fn participant_volume_popup_id(scope: VolumeMenuScope, identity: &str) -> egui::Id {
    egui::Id::unique(("participant_volume_popup", scope, identity))
}

fn participant_volume_badge(volume: u16, is_self: bool) -> Option<String> {
    if is_self || volume == DEFAULT_PARTICIPANT_VOLUME {
        None
    } else {
        Some(format!("{volume}%"))
    }
}

fn participant_volume_menu(
    card_response: &egui::Response,
    scope: VolumeMenuScope,
    identity: &str,
    current_volume: u16,
    actions: &mut Vec<UserAction>,
) {
    let ctx = &card_response.ctx;
    let popup_id = participant_volume_popup_id(scope, identity);
    let dirty_id = popup_id.with("dirty");
    let mut needs_save = ctx.data_mut(|d| d.get_temp::<bool>(dirty_id).unwrap_or(false));

    let popup = egui::Popup::menu(card_response)
        .id(popup_id)
        .open_memory(if card_response.container_secondary_clicked() {
            Some(egui::SetOpenCommand::Bool(true))
        } else if card_response.container_clicked() {
            Some(egui::SetOpenCommand::Bool(false))
        } else {
            None
        })
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .at_pointer_fixed()
        .show(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(
                RichText::new("Volume")
                    .font(theme::semibold(13.0))
                    .color(theme::TEXT),
            );
            ui.spacing_mut().slider_width = 160.0;
            let mut value = current_volume;
            let slider = ui.add(
                egui::Slider::new(&mut value, 0..=MAX_PARTICIPANT_VOLUME)
                    .step_by(5.0)
                    .suffix("%"),
            );
            if value != current_volume {
                actions.push(UserAction::SetParticipantVolume {
                    identity: identity.to_owned(),
                    percent: value,
                });
                needs_save = true;
            }
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                actions.push(UserAction::SaveAudioSettings);
                needs_save = false;
            }
            if ui.button("Restaurar 100%").clicked() {
                if current_volume != DEFAULT_PARTICIPANT_VOLUME
                    || value != DEFAULT_PARTICIPANT_VOLUME
                {
                    actions.push(UserAction::SetParticipantVolume {
                        identity: identity.to_owned(),
                        percent: DEFAULT_PARTICIPANT_VOLUME,
                    });
                }
                actions.push(UserAction::SaveAudioSettings);
                needs_save = false;
                ui.close();
            }
        });

    let closing = popup.as_ref().is_some_and(|p| p.response.should_close())
        || !egui::Popup::is_id_open(ctx, popup_id);
    if needs_save && closing {
        actions.push(UserAction::SaveAudioSettings);
        needs_save = false;
    }
    ctx.data_mut(|d| {
        if needs_save {
            d.insert_temp(dirty_id, true);
        } else {
            d.remove::<bool>(dirty_id);
        }
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

fn occupant_volume_target<'a>(
    is_current_room: bool,
    name: &str,
    summary_participants: &[String],
    current_participants: &'a [ParticipantView],
    local_identity: &str,
) -> Option<&'a ParticipantView> {
    if !is_current_room {
        return None;
    }
    if summary_participants
        .iter()
        .filter(|candidate| candidate.as_str() == name)
        .count()
        != 1
    {
        return None;
    }
    let mut matches = current_participants.iter().filter(|p| p.name == name);
    let target = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let is_self = !local_identity.is_empty() && target.identity == local_identity;
    if is_self { None } else { Some(target) }
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

    fn participant(identity: &str, name: &str) -> ParticipantView {
        ParticipantView {
            identity: identity.to_owned(),
            name: name.to_owned(),
            muted: false,
            speaking: false,
        }
    }

    #[test]
    fn formats_people_count_in_portuguese() {
        assert_eq!(people_count_label(0), "0 pessoas na sala");
        assert_eq!(people_count_label(1), "1 pessoa na sala");
        assert_eq!(people_count_label(4), "4 pessoas na sala");
    }

    #[test]
    fn resolves_local_display_name_from_participants_or_fallback() {
        let list = vec![participant("id-1", "Lucas")];
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

    #[test]
    fn participant_volume_badge_shows_only_for_non_default_remote_participants() {
        assert_eq!(participant_volume_badge(100, false), None);
        assert_eq!(participant_volume_badge(70, false), Some("70%".to_owned()));
        assert_eq!(
            participant_volume_badge(165, false),
            Some("165%".to_owned())
        );
        assert_eq!(participant_volume_badge(70, true), None);
    }

    #[test]
    fn occupant_volume_target_matches_unique_remote_in_current_room() {
        let summary = vec!["Lucas".to_owned(), "Ana".to_owned()];
        let participants = vec![
            participant("id-self", "Lucas"),
            participant("id-ana", "Ana"),
        ];

        let target = occupant_volume_target(true, "Ana", &summary, &participants, "id-self");
        assert_eq!(target.map(|p| p.identity.as_str()), Some("id-ana"));
        assert!(
            occupant_volume_target(true, "Lucas", &summary, &participants, "id-self").is_none()
        );
        assert!(occupant_volume_target(false, "Ana", &summary, &participants, "id-self").is_none());
        assert!(
            occupant_volume_target(true, "Bruno", &summary, &participants, "id-self").is_none()
        );
    }

    #[test]
    fn occupant_volume_target_skips_duplicate_names_in_participants_or_summary() {
        let summary = vec!["Ana".to_owned(), "Ana".to_owned(), "Bruno".to_owned()];
        let two_remotes = vec![
            participant("id-self", "Lucas"),
            participant("id-ana-1", "Ana"),
            participant("id-ana-2", "Ana"),
            participant("id-bruno", "Bruno"),
        ];
        assert!(occupant_volume_target(true, "Ana", &summary, &two_remotes, "id-self").is_none());
        assert_eq!(
            occupant_volume_target(true, "Bruno", &summary, &two_remotes, "id-self")
                .map(|p| p.identity.as_str()),
            Some("id-bruno")
        );

        let summary_single_ana = vec!["Ana".to_owned()];
        let self_and_remote_same_name = vec![
            participant("id-self", "Ana"),
            participant("id-remote-ana", "Ana"),
        ];
        assert!(
            occupant_volume_target(
                true,
                "Ana",
                &summary_single_ana,
                &self_and_remote_same_name,
                "id-self"
            )
            .is_none()
        );

        let single_remote = vec![participant("id-ana-1", "Ana")];
        assert!(occupant_volume_target(true, "Ana", &summary, &single_remote, "id-self").is_none());
    }

    #[test]
    fn participant_volume_popup_ids_are_stable_and_distinct_per_scope() {
        let sidebar_a1 = participant_volume_popup_id(VolumeMenuScope::Sidebar, "id-ana");
        let sidebar_a2 = participant_volume_popup_id(VolumeMenuScope::Sidebar, "id-ana");
        let grid_a = participant_volume_popup_id(VolumeMenuScope::Grid, "id-ana");
        let strip_a = participant_volume_popup_id(VolumeMenuScope::Strip, "id-ana");
        let sidebar_b = participant_volume_popup_id(VolumeMenuScope::Sidebar, "id-bruno");

        assert_eq!(sidebar_a1, sidebar_a2);
        assert_ne!(sidebar_a1, grid_a);
        assert_ne!(sidebar_a1, strip_a);
        assert_ne!(grid_a, strip_a);
        assert_ne!(sidebar_a1, sidebar_b);
    }
}
