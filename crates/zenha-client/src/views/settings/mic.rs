//! Medidor do microfone e noise gate.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MicMeterView {
    Level(MicLevelView),
    Message(&'static str),
}

pub(super) fn mic_meter_view(
    in_room: bool,
    mic_muted: bool,
    mic_level: Option<MicLevelView>,
) -> MicMeterView {
    if !in_room {
        MicMeterView::Message("Entre em uma sala para ver o nível do microfone.")
    } else if mic_muted {
        MicMeterView::Message("Microfone desligado.")
    } else if let Some(level) = mic_level {
        MicMeterView::Level(level)
    } else {
        MicMeterView::Message("Aguardando sinal do microfone…")
    }
}

pub(super) fn noise_gate(
    ui: &mut egui::Ui,
    audio: &AudioState,
    in_room: bool,
    mic_muted: bool,
    actions: &mut Vec<UserAction>,
) {
    let mut enabled = audio.noise_gate.is_some();
    if ui
        .checkbox(
            &mut enabled,
            "Porta de ruído (corta o som abaixo do limiar)",
        )
        .changed()
    {
        actions.push(UserAction::SetNoiseGate(enabled.then_some(GATE_DEFAULT_DB)));
        actions.push(UserAction::SaveAudioSettings);
    }
    ui.add_space(ROW_GAP);

    let current = audio.noise_gate.unwrap_or(GATE_DEFAULT_DB);
    let mut value = current;
    let meter = mic_meter_view(in_room, mic_muted, audio.mic_level);
    egui::Grid::new("noise_gate_grid")
        .num_columns(3)
        .spacing([ROW_GAP, ROW_GAP])
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(GATE_LABEL_WIDTH, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label("Sensibilidade");
                },
            );
            let slider = ui
                .scope(|ui| {
                    ui.spacing_mut().slider_width = GATE_CONTROL_WIDTH;
                    ui.add_enabled(
                        enabled,
                        egui::Slider::new(&mut value, GATE_MIN_DB..=GATE_MAX_DB).show_value(false),
                    )
                })
                .inner;
            if value != current {
                actions.push(UserAction::SetNoiseGate(Some(value)));
            }
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                actions.push(UserAction::SaveAudioSettings);
            }
            db_value_cell(ui, value, enabled);
            ui.end_row();

            if let MicMeterView::Level(MicLevelView {
                level_db,
                gate_open,
            }) = meter
            {
                ui.allocate_ui_with_layout(
                    egui::vec2(GATE_LABEL_WIDTH, ui.spacing().interact_size.y),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.label("Nível do microfone");
                    },
                );
                let color = if gate_open {
                    theme::SPEAKING
                } else {
                    theme::INACTIVE_DOT
                };
                ui.add(
                    egui::ProgressBar::new(meter_fraction(level_db))
                        .desired_width(GATE_CONTROL_WIDTH)
                        .fill(color),
                );
                db_value_cell(ui, level_db, true);
                ui.end_row();
            }
        });
    ui.add_space(CONTROL_GAP);
    ui.small("Mais para a direita corta mais ruído, mas pode cortar fala baixa.");
    if let MicMeterView::Message(message) = meter {
        ui.add_space(CONTROL_GAP);
        ui.small(message);
    }
}

fn db_value_cell(ui: &mut egui::Ui, db: i8, enabled: bool) {
    let color = if enabled {
        theme::TEXT_SECONDARY
    } else {
        theme::TEXT_WEAK
    };
    ui.allocate_ui_with_layout(
        egui::vec2(GATE_VALUE_WIDTH, ui.spacing().interact_size.y),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.label(RichText::new(format!("{db} dB")).monospace().color(color));
        },
    );
}

pub(super) fn meter_fraction(level_db: i8) -> f32 {
    ((f32::from(level_db) - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}
