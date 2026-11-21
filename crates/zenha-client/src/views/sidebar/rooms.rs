//! Sidebar room list and its occupant rows.

use std::collections::BTreeMap;

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Vec2};
use zenha_protocol::{RoomSummary, VoiceState};

use crate::icons;
use crate::state::{ShareState, UserAction, participant_volume_for, sidebar_order};
use crate::theme;
use crate::ui_model::ParticipantView;
use crate::views::participant_menu::{
    ParticipantMenuCtx, VolumeMenuScope, participant_volume_badge, participant_volume_menu,
};

pub(super) struct RoomsListView<'a> {
    pub(super) current_room: Option<&'a str>,
    pub(super) local_identity: &'a str,
    pub(super) participants: &'a [ParticipantView],
    pub(super) share: Option<&'a ShareState>,
    pub(super) participant_volumes: &'a BTreeMap<String, u16>,
    pub(super) rooms: &'a [RoomSummary],
    pub(super) is_admin: bool,
}

pub(super) fn rooms_list(
    ui: &mut egui::Ui,
    view: RoomsListView<'_>,
    actions: &mut Vec<UserAction>,
) {
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
            for (room_idx, summary, depth) in sidebar_order(view.rooms) {
                let is_current = view.current_room == Some(summary.name.as_str());
                let row_resp = room_row(
                    ui,
                    summary.display_title(),
                    summary.occupant_count(),
                    is_current,
                    summary.locked,
                    f32::from(depth) * CHILD_INDENT,
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
                        if ui.button("Editar").clicked() {
                            actions.push(UserAction::OpenEditRoom(summary.name.clone()));
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
                let occupants = visible_occupants(summary, is_current, view.participants);
                if !occupants.is_empty() {
                    ui.add_space(2.0);
                    let names: Vec<&str> =
                        occupants.iter().map(|(name, _)| name.as_str()).collect();
                    for (idx, (name, voice)) in occupants.iter().enumerate() {
                        let speaking = is_occupant_speaking(is_current, name, view.participants);
                        let volume_target = occupant_volume_target(
                            is_current,
                            name,
                            &names,
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
                                *voice,
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

const CHILD_INDENT: f32 = 16.0;

fn room_row(
    ui: &mut egui::Ui,
    name: &str,
    count: usize,
    is_current: bool,
    locked: bool,
    indent: f32,
) -> egui::Response {
    let desired = Vec2::new(ui.available_width(), 44.0);
    let (full, response) = ui.allocate_exact_size(desired, Sense::click());
    let rect = full.with_min_x(full.left() + indent);
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
        let count_space = if locked { 54.0 } else { 36.0 };
        let left_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 12.0, rect.top()),
            egui::pos2(
                (rect.right() - count_space).max(rect.left() + 32.0),
                rect.bottom(),
            ),
        );
        if locked {
            let icon = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 40.0, rect.center().y),
                Vec2::splat(13.0),
            );
            icons::padlock(&painter, icon, count_color);
        }
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

const VOICE_ICON_SIZE: f32 = 13.0;

fn occupant_row(
    ui: &mut egui::Ui,
    name: &str,
    voice: VoiceState,
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
                let icon_count = u8::from(voice.muted) + u8::from(voice.deafened);
                let icons_width = f32::from(icon_count) * (VOICE_ICON_SIZE + 4.0);
                let badge_width = if volume_badge.is_some() { 44.0 } else { 0.0 };
                let max_name_width = (ui.available_width() - badge_width - icons_width).max(24.0);
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
                if voice.muted {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::splat(VOICE_ICON_SIZE), Sense::hover());
                    icons::mic(ui.painter(), rect, theme::TEXT_WEAK, true);
                    response.on_hover_text("Microfone desligado");
                }
                if voice.deafened {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::splat(VOICE_ICON_SIZE), Sense::hover());
                    icons::headphones(ui.painter(), rect, theme::TEXT_WEAK, true);
                    response.on_hover_text("Fone desligado");
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
    summary_participants: &[&str],
    current_participants: &'a [ParticipantView],
    local_identity: &str,
) -> Option<&'a ParticipantView> {
    if !is_current_room {
        return None;
    }
    if summary_participants
        .iter()
        .filter(|candidate| **candidate == name)
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

/// A locked room the server hides is still listed for whoever is inside it, from the live participants.
/// In the current room the live microphone and headphones state wins over the last `/rooms` poll.
fn visible_occupants(
    summary: &RoomSummary,
    is_current: bool,
    participants: &[ParticipantView],
) -> Vec<(String, VoiceState)> {
    let live_voice = |p: &ParticipantView| VoiceState {
        muted: p.muted,
        deafened: p.deafened,
    };
    if is_current && summary.locked && summary.participants.is_empty() {
        return participants
            .iter()
            .map(|p| (p.name.clone(), live_voice(p)))
            .collect();
    }
    summary
        .participants
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            let live = participants
                .iter()
                .find(|p| is_current && p.name == *name)
                .map(live_voice);
            (name.clone(), live.unwrap_or_else(|| summary.voice_of(idx)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn participant(identity: &str, name: &str) -> ParticipantView {
        ParticipantView {
            identity: identity.to_owned(),
            name: name.to_owned(),
            muted: false,
            deafened: false,
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
            deafened: false,
            speaking: true,
            avatar_version: None,
        }];
        assert!(is_occupant_speaking(true, "Ana", &list));
        assert!(!is_occupant_speaking(false, "Ana", &list));
        assert!(!is_occupant_speaking(true, "Bruno", &list));
    }

    #[test]
    fn locked_room_lists_live_participants_only_for_whoever_is_inside() {
        let hidden = RoomSummary {
            name: "vip".to_owned(),
            participant_count: 2,
            locked: true,
            ..RoomSummary::default()
        };
        let live = vec![
            participant("id-self", "Lucas"),
            participant("id-ana", "Ana"),
        ];
        let names = |occupants: Vec<(String, VoiceState)>| -> Vec<String> {
            occupants.into_iter().map(|(name, _)| name).collect()
        };
        assert_eq!(
            names(visible_occupants(&hidden, true, &live)),
            vec!["Lucas", "Ana"]
        );
        assert!(visible_occupants(&hidden, false, &live).is_empty());
        let open = RoomSummary {
            participants: vec!["Bia".to_owned()],
            ..hidden.clone()
        };
        assert_eq!(names(visible_occupants(&open, true, &live)), vec!["Bia"]);
    }

    #[test]
    fn occupant_voice_comes_from_the_live_list_in_the_current_room_and_the_poll_elsewhere() {
        let off = VoiceState {
            muted: true,
            deafened: true,
        };
        let summary = RoomSummary {
            name: "geral".to_owned(),
            participants: vec!["Ana".to_owned(), "Bia".to_owned()],
            voice: vec![VoiceState::default(), off],
            ..RoomSummary::default()
        };
        let mut ana = participant("id-ana", "Ana");
        ana.deafened = true;
        let live = vec![ana, participant("id-bia", "Bia")];
        let voices = |is_current| -> Vec<VoiceState> {
            visible_occupants(&summary, is_current, &live)
                .into_iter()
                .map(|(_, voice)| voice)
                .collect()
        };
        assert_eq!(
            voices(false),
            vec![VoiceState::default(), off],
            "another room uses the polled state"
        );
        assert_eq!(
            voices(true),
            vec![
                VoiceState {
                    muted: false,
                    deafened: true
                },
                VoiceState::default()
            ],
            "the current room uses the live state"
        );
        let old_server = RoomSummary {
            voice: Vec::new(),
            ..summary.clone()
        };
        assert!(
            visible_occupants(&old_server, false, &live)
                .iter()
                .all(|(_, voice)| *voice == VoiceState::default())
        );
    }

    #[test]
    fn occupant_volume_target_matches_unique_remote_in_current_room() {
        let summary = vec!["Lucas", "Ana"];
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
        let summary = vec!["Ana", "Ana", "Bruno"];
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

        let summary_single_ana = vec!["Ana"];
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
}
