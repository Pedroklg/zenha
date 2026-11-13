use eframe::egui;
use rooms_protocol::RoomSummary;

use super::chat;
use super::share::VideoRenderer;
use crate::chat_images::ChatImages;
use crate::state::{LocalShare, RoomState, UserAction};

pub fn show(
    ui: &mut egui::Ui,
    state: &mut RoomState,
    rooms: &[RoomSummary],
    renderer: &mut VideoRenderer,
    images: &mut ChatImages,
    stream_volume: u8,
    ptt_hint: Option<&str>,
) -> Vec<UserAction> {
    let mut actions = Vec::new();

    if let Some(watch) = state.share.watching.as_ref().filter(|w| w.fullscreen) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
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

    egui::Panel::bottom("bottom_bar").show(ui, |ui| {
        ui.add_space(4.0);
        bottom_bar(ui, state, ptt_hint, &mut actions);
        ui.add_space(4.0);
    });

    egui::Panel::left("rooms_panel")
        .default_size(200.0)
        .size_range(150.0..=320.0)
        .show(ui, |ui| rooms_list(ui, &state.room, rooms, &mut actions));

    if state.chat.open {
        actions.extend(chat::show(ui, &state.room, &mut state.chat, images));
    }

    egui::CentralPanel::default().show(ui, |ui| {
        if let Some(error) = state.share.error.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), error);
                if ui.small_button("Fechar").clicked() {
                    actions.push(UserAction::DismissShareError);
                }
            });
        }
        if let Some(watch) = &state.share.watching {
            watching(ui, state, watch, renderer, stream_volume, &mut actions);
        } else {
            participants(ui, state, &mut actions);
        }
    });
    actions
}

fn rooms_list(
    ui: &mut egui::Ui,
    current: &str,
    rooms: &[RoomSummary],
    actions: &mut Vec<UserAction>,
) {
    ui.heading("Salas");
    ui.separator();
    if rooms.is_empty() {
        ui.weak("Carregando salas...");
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for summary in rooms {
            let is_current = summary.name == current;
            let label = format!("{} ({})", summary.name, summary.participants.len());
            if ui.selectable_label(is_current, label).clicked() && !is_current {
                actions.push(UserAction::SwitchRoom(summary.name.clone()));
            }
            ui.indent(("occupants", &summary.name), |ui| {
                for name in &summary.participants {
                    ui.weak(name);
                }
            });
        }
    });
}

fn watching(
    ui: &mut egui::Ui,
    state: &RoomState,
    watch: &crate::state::WatchState,
    renderer: &mut VideoRenderer,
    stream_volume: u8,
    actions: &mut Vec<UserAction>,
) {
    let name = state
        .participants
        .iter()
        .find(|p| p.identity == watch.identity)
        .map_or(watch.identity.as_str(), |p| p.name.as_str());
    ui.horizontal(|ui| {
        ui.heading(format!("Tela de {name}"));
        if ui.button("Parar de assistir").clicked() {
            actions.push(UserAction::StopWatching);
        }
        if ui.button("Tela cheia").clicked() {
            actions.push(UserAction::ToggleFullscreen);
        }
        ui.separator();
        ui.label("Som da transmissão");
        let mut volume = stream_volume;
        let slider = ui.add(egui::Slider::new(&mut volume, 0..=100).suffix("%"));
        if volume != stream_volume {
            actions.push(UserAction::SetStreamVolume(volume));
        }
        if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
            actions.push(UserAction::SaveAudioSettings);
        }
    });
    ui.horizontal_wrapped(|ui| {
        for participant in &state.participants {
            let text = if participant.muted {
                format!("{} (mudo)", participant.name)
            } else {
                participant.name.clone()
            };
            if participant.speaking {
                ui.colored_label(egui::Color32::from_rgb(60, 180, 90), format!("● {text}"));
            } else {
                ui.weak(text);
            }
        }
    });
    ui.separator();
    renderer.show(ui, watch);
}

fn participants(ui: &mut egui::Ui, state: &RoomState, actions: &mut Vec<UserAction>) {
    ui.heading(format!("Sala: {}", state.room));
    ui.separator();
    egui::ScrollArea::vertical().show(ui, |ui| {
        for participant in &state.participants {
            let fill = if participant.speaking {
                egui::Color32::from_rgba_unmultiplied(60, 180, 90, 70)
            } else {
                egui::Color32::TRANSPARENT
            };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(4)
                .inner_margin(6)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if participant.speaking {
                            ui.colored_label(egui::Color32::from_rgb(60, 180, 90), "●");
                        }
                        ui.label(&participant.name);
                        if participant.muted {
                            ui.weak("(mudo)");
                        }
                        if state.share.is_sharing_remote(&participant.identity) {
                            ui.weak("transmitindo tela");
                            if ui.button("Assistir").clicked() {
                                actions.push(UserAction::Watch(participant.identity.clone()));
                            }
                        }
                    });
                });
        }
    });
}

fn bottom_bar(
    ui: &mut egui::Ui,
    state: &RoomState,
    ptt_hint: Option<&str>,
    actions: &mut Vec<UserAction>,
) {
    ui.horizontal(|ui| {
        let mute_text = if state.muted {
            "Microfone mudo"
        } else {
            "Microfone ligado"
        };
        let mute = egui::Button::new(mute_text).selected(state.muted);
        if ui.add_enabled(!state.deafened, mute).clicked() {
            actions.push(UserAction::ToggleMute);
        }
        if let Some(hint) = ptt_hint {
            ui.small(hint);
        }

        let deafen_text = if state.deafened {
            "Áudio desligado"
        } else {
            "Áudio ligado"
        };
        if ui
            .add(egui::Button::new(deafen_text).selected(state.deafened))
            .clicked()
        {
            actions.push(UserAction::ToggleDeafen);
        }

        let chat_text = if state.chat.unread > 0 {
            format!("Chat ({})", state.chat.unread)
        } else {
            "Chat".to_owned()
        };
        if ui
            .add(egui::Button::new(chat_text).selected(state.chat.open))
            .clicked()
        {
            actions.push(UserAction::ToggleChat);
        }

        match state.share.local {
            LocalShare::Idle => {
                if ui.button("Transmitir tela").clicked() {
                    actions.push(UserAction::OpenSharePicker);
                }
            }
            LocalShare::Starting => {
                ui.add_enabled(false, egui::Button::new("Iniciando transmissão..."));
            }
            LocalShare::Active => {
                if ui.button("Parar transmissão").clicked() {
                    actions.push(UserAction::StopShare);
                }
            }
        }

        if ui.button("Configurações").clicked() {
            actions.push(UserAction::OpenSettings);
        }

        if ui.button("Sair").clicked() {
            actions.push(UserAction::Leave);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(state.status.label());
        });
    });
}
