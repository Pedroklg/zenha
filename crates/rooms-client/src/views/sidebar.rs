//! Barra lateral com lista de salas, ocupantes e rodapé do usuário.

use std::collections::BTreeMap;

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Vec2,
};
use rooms_protocol::RoomSummary;

use super::participant_menu::{
    ParticipantMenuCtx, VolumeMenuScope, participant_volume_badge, participant_volume_menu,
};
use super::room::HEADER_HEIGHT;
use crate::format::format_size;
use crate::icons;
use crate::state::{
    ConnectionStatus, ShareState, UpdateBanner, UpdatePhase, UserAction, participant_volume_for,
};
use crate::theme;
use crate::ui_model::ParticipantView;

const SIDEBAR_WIDTH: f32 = 256.0;
const FOOTER_AVATAR_SIZE: f32 = 32.0;
const FOOTER_BUTTON_SIZE: f32 = 32.0;
const FOOTER_BUTTON_GAP: f32 = 4.0;

pub(super) struct SidebarUser<'a> {
    pub(super) identity: &'a str,
    pub(super) avatar_version: Option<&'a str>,
    pub(super) name: &'a str,
    pub(super) connection: Option<ConnectionStatus>,
    pub(super) ptt_hint: Option<&'a str>,
    pub(super) muted: bool,
    pub(super) deafened: bool,
}

pub(super) struct SidebarArgs<'a> {
    pub(super) user: SidebarUser<'a>,
    pub(super) current_room: Option<&'a str>,
    pub(super) local_identity: &'a str,
    pub(super) participants: &'a [ParticipantView],
    pub(super) share: Option<&'a ShareState>,
    pub(super) participant_volumes: &'a BTreeMap<String, u16>,
    pub(super) rooms: &'a [RoomSummary],
    pub(super) server_host: &'a str,
    pub(super) update_banner: Option<&'a UpdateBanner>,
    pub(super) is_admin: bool,
}

pub(super) fn sidebar(ui: &mut egui::Ui, args: SidebarArgs<'_>, actions: &mut Vec<UserAction>) {
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
                    if let Some(banner) = args.update_banner {
                        ui.add_space(8.0);
                        update_footer(ui, banner, actions);
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
                            share: args.share,
                            participant_volumes: args.participant_volumes,
                            rooms: args.rooms,
                            is_admin: args.is_admin,
                        },
                        actions,
                    );
                });
        });
}

fn update_footer(ui: &mut egui::Ui, banner: &UpdateBanner, actions: &mut Vec<UserAction>) {
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

fn user_footer(ui: &mut egui::Ui, user: SidebarUser<'_>, actions: &mut Vec<UserAction>) {
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

struct RoomsListView<'a> {
    current_room: Option<&'a str>,
    local_identity: &'a str,
    participants: &'a [ParticipantView],
    share: Option<&'a ShareState>,
    participant_volumes: &'a BTreeMap<String, u16>,
    rooms: &'a [RoomSummary],
    is_admin: bool,
}

fn rooms_list(ui: &mut egui::Ui, view: RoomsListView<'_>, actions: &mut Vec<UserAction>) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("SALAS")
                .font(theme::semibold(11.0))
                .color(theme::TEXT_WEAK),
        );
        if view.is_admin {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let add = egui::Button::new(RichText::new("+").font(theme::semibold(13.0)))
                    .small()
                    .min_size(Vec2::new(22.0, 20.0));
                if ui.add(add).on_hover_text("Criar sala").clicked() {
                    actions.push(UserAction::OpenCreateRoom);
                }
            });
        }
    });
    ui.add_space(6.0);

    if view.rooms.is_empty() {
        ui.weak("Carregando salas...");
    }

    let room_count = view.rooms.len();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (room_idx, summary) in view.rooms.iter().enumerate() {
                let is_current = view.current_room == Some(summary.name.as_str());
                let row_resp = room_row(
                    ui,
                    summary.display_title(),
                    summary.participants.len(),
                    is_current,
                );
                if row_resp.clicked() && !is_current {
                    let action = if view.current_room.is_some() {
                        UserAction::SwitchRoom(summary.name.clone())
                    } else {
                        UserAction::JoinRoom(summary.name.clone())
                    };
                    actions.push(action);
                }
                if view.is_admin {
                    row_resp.context_menu(|ui| {
                        if ui.button("Renomear").clicked() {
                            actions.push(UserAction::OpenRenameRoom(summary.name.clone()));
                            ui.close();
                        }
                        if ui
                            .add_enabled(room_idx > 0, egui::Button::new("Mover para cima"))
                            .clicked()
                        {
                            actions.push(UserAction::MoveRoomUp(summary.name.clone()));
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                room_idx + 1 < room_count,
                                egui::Button::new("Mover para baixo"),
                            )
                            .clicked()
                        {
                            actions.push(UserAction::MoveRoomDown(summary.name.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .button(RichText::new("Apagar").color(theme::ERROR))
                            .clicked()
                        {
                            actions.push(UserAction::OpenDeleteRoom(summary.name.clone()));
                            ui.close();
                        }
                    });
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
                            occupant_row(
                                ui,
                                name,
                                speaking,
                                volume_target,
                                OccupantAdminCtx {
                                    is_admin: view.is_admin,
                                    share: view.share,
                                    current_room: &summary.name,
                                    rooms: view.rooms,
                                },
                                actions,
                            );
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
        let painter = ui.painter().clone();
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
        let left_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 12.0, rect.top()),
            egui::pos2((rect.right() - 36.0).max(rect.left() + 32.0), rect.bottom()),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
            ui.set_clip_rect(left_rect);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(RichText::new(name).font(name_font).color(name_color))
                        .truncate()
                        .selectable(false),
                );
            });
        });
        let right_pos = egui::pos2(rect.right() - 12.0, rect.center().y);
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

struct OccupantAdminCtx<'a> {
    is_admin: bool,
    share: Option<&'a ShareState>,
    current_room: &'a str,
    rooms: &'a [RoomSummary],
}

fn occupant_row(
    ui: &mut egui::Ui,
    name: &str,
    speaking: bool,
    volume_target: Option<(&ParticipantView, u16)>,
    admin: OccupantAdminCtx<'_>,
    actions: &mut Vec<UserAction>,
) {
    let interactive = volume_target.is_some();
    let sense = if interactive {
        Sense::click()
    } else {
        Sense::hover()
    };
    let row_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        Vec2::new(ui.available_width(), ui.spacing().interact_size.y),
    );
    let row_response = ui
        .scope_builder(
            egui::UiBuilder::new()
                .max_rect(row_rect)
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
        let is_sharing = admin
            .share
            .is_some_and(|share| share.is_sharing_remote(&participant.identity));
        participant_volume_menu(
            &row_response,
            ParticipantMenuCtx {
                scope: VolumeMenuScope::Sidebar,
                identity: &participant.identity,
                current_volume: volume,
                is_admin: admin.is_admin,
                is_sharing,
                current_room: admin.current_room,
                rooms: admin.rooms,
            },
            actions,
        );
    }
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
            avatar_version: None,
        }
    }

    #[test]
    fn occupant_speaking_is_true_only_in_current_room_when_speaking() {
        let list = vec![ParticipantView {
            identity: "id-ana".to_owned(),
            name: "Ana".to_owned(),
            muted: false,
            speaking: true,
            avatar_version: None,
        }];
        assert!(is_occupant_speaking(true, "Ana", &list));
        assert!(!is_occupant_speaking(false, "Ana", &list));
        assert!(!is_occupant_speaking(true, "Bruno", &list));
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
