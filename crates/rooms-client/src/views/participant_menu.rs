//! Menu contextual de volume individual e ações administrativas sobre participantes.

use eframe::egui::{self, RichText};
use rooms_protocol::RoomSummary;

use crate::state::{DEFAULT_PARTICIPANT_VOLUME, MAX_PARTICIPANT_VOLUME, UserAction};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum VolumeMenuScope {
    Sidebar,
    Grid,
    Strip,
}

fn participant_volume_popup_id(scope: VolumeMenuScope, identity: &str) -> egui::Id {
    egui::Id::unique(("participant_volume_popup", scope, identity))
}

pub(super) fn participant_volume_badge(volume: u16, is_self: bool) -> Option<String> {
    if is_self || volume == DEFAULT_PARTICIPANT_VOLUME {
        None
    } else {
        Some(format!("{volume}%"))
    }
}

pub(super) struct ParticipantMenuCtx<'a> {
    pub(super) scope: VolumeMenuScope,
    pub(super) identity: &'a str,
    pub(super) current_volume: u16,
    pub(super) is_admin: bool,
    pub(super) is_sharing: bool,
    pub(super) current_room: &'a str,
    pub(super) rooms: &'a [RoomSummary],
}

pub(super) fn participant_volume_menu(
    card_response: &egui::Response,
    menu: ParticipantMenuCtx<'_>,
    actions: &mut Vec<UserAction>,
) {
    let ctx = &card_response.ctx;
    let popup_id = participant_volume_popup_id(menu.scope, menu.identity);
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
            let mut value = menu.current_volume;
            let slider = ui.add(
                egui::Slider::new(&mut value, 0..=MAX_PARTICIPANT_VOLUME)
                    .step_by(5.0)
                    .suffix("%"),
            );
            if value != menu.current_volume {
                actions.push(UserAction::SetParticipantVolume {
                    identity: menu.identity.to_owned(),
                    percent: value,
                });
                needs_save = true;
            }
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                actions.push(UserAction::SaveAudioSettings);
                needs_save = false;
            }
            if ui.button("Restaurar 100%").clicked() {
                if menu.current_volume != DEFAULT_PARTICIPANT_VOLUME
                    || value != DEFAULT_PARTICIPANT_VOLUME
                {
                    actions.push(UserAction::SetParticipantVolume {
                        identity: menu.identity.to_owned(),
                        percent: DEFAULT_PARTICIPANT_VOLUME,
                    });
                }
                actions.push(UserAction::SaveAudioSettings);
                needs_save = false;
                ui.close();
            }

            if menu.is_admin {
                ui.separator();
                if ui.button("Mutar microfone").clicked() {
                    actions.push(UserAction::AdminMuteParticipant {
                        identity: menu.identity.to_owned(),
                    });
                    ui.close();
                }
                if menu.is_sharing && ui.button("Parar transmissão").clicked() {
                    actions.push(UserAction::AdminStopShareParticipant {
                        identity: menu.identity.to_owned(),
                    });
                    ui.close();
                }
                let other_rooms: Vec<&RoomSummary> = menu
                    .rooms
                    .iter()
                    .filter(|r| r.name != menu.current_room)
                    .collect();
                if !other_rooms.is_empty() {
                    ui.menu_button("Mover para…", |ui| {
                        for target in other_rooms {
                            if ui.button(target.display_title()).clicked() {
                                actions.push(UserAction::AdminMoveParticipant {
                                    identity: menu.identity.to_owned(),
                                    to_room: target.name.clone(),
                                });
                                ui.close();
                            }
                        }
                    });
                }
                if ui
                    .button(RichText::new("Remover da sala").color(theme::ERROR))
                    .clicked()
                {
                    actions.push(UserAction::OpenKickParticipant {
                        identity: menu.identity.to_owned(),
                    });
                    ui.close();
                }
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

#[cfg(test)]
mod tests {
    use super::*;

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
