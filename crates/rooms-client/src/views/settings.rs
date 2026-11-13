use eframe::egui;

use crate::hotkeys::{HotkeyAction, binding_from_key, binding_label};
use crate::settings::NoiseMode;
use crate::state::{AudioState, DEFAULT_DEVICE_LABEL, HotkeyState, MicLevelView, UserAction};
use crate::ui_model::AudioDeviceView;

const COMBO_WIDTH: f32 = 320.0;
const GATE_MIN_DB: i8 = -70;
const GATE_MAX_DB: i8 = -20;
const GATE_DEFAULT_DB: i8 = -45;
const METER_FLOOR_DB: f32 = -70.0;
const OPEN_COLOR: egui::Color32 = egui::Color32::from_rgb(60, 180, 90);
const CLOSED_COLOR: egui::Color32 = egui::Color32::from_rgb(130, 130, 130);
const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);

pub fn show(ctx: &egui::Context, audio: &AudioState, hotkeys: &HotkeyState) -> Vec<UserAction> {
    let mut actions = Vec::new();
    if !audio.open {
        return actions;
    }
    let mut open = true;
    egui::Window::new("Configurações")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| content(ui, audio, hotkeys, &mut actions));
    if !open {
        actions.push(UserAction::CloseSettings);
    }
    actions
}

fn content(
    ui: &mut egui::Ui,
    audio: &AudioState,
    hotkeys: &HotkeyState,
    actions: &mut Vec<UserAction>,
) {
    ui.heading("Áudio");
    ui.add_space(8.0);

    ui.label("Microfone");
    device_combo(
        ui,
        "input_device",
        &audio.input_label(),
        &audio.inputs,
        audio.input.as_deref(),
        UserAction::SelectInputDevice,
        actions,
    );
    ui.add_space(8.0);

    ui.label("Saída de áudio");
    device_combo(
        ui,
        "output_device",
        &audio.output_label(),
        &audio.outputs,
        audio.output.as_deref(),
        UserAction::SelectOutputDevice,
        actions,
    );
    ui.add_space(8.0);

    ui.label("Supressão de ruído");
    egui::ComboBox::from_id_salt("noise_mode")
        .selected_text(audio.noise.label())
        .width(COMBO_WIDTH)
        .show_ui(ui, |ui| {
            for mode in NoiseMode::ALL {
                if ui
                    .selectable_label(audio.noise == mode, mode.label())
                    .clicked()
                {
                    actions.push(UserAction::SelectNoiseMode(mode));
                }
            }
        });
    if audio.noise.uses_own_capture() {
        ui.small(match audio.noise {
            NoiseMode::Maximum => {
                "Máxima: rede mais pesada (DeepFilterNet), usa mais CPU. Sem cancelamento de eco, use fone."
            }
            _ => "Reforçada: melhor contra teclado e ventilador. Sem cancelamento de eco, use fone.",
        });
        ui.add_space(8.0);
        noise_gate(ui, audio, actions);
    }
    ui.add_space(12.0);

    if ui.button("Atualizar lista").clicked() {
        actions.push(UserAction::RefreshAudioDevices);
    }

    ui.separator();
    ui.heading("Atalhos globais");
    ui.small("Funcionam com o app minimizado e com um jogo em foco.");
    hotkeys_section(ui, hotkeys, actions);
}

fn hotkeys_section(ui: &mut egui::Ui, hotkeys: &HotkeyState, actions: &mut Vec<UserAction>) {
    if let Some(capturing) = hotkeys.capturing {
        for event in ui.input(|i| i.events.clone()) {
            if let egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            {
                let key = physical_key.unwrap_or(key);
                if key == egui::Key::Escape {
                    actions.push(UserAction::CancelHotkeyCapture);
                } else {
                    match binding_from_key(key, modifiers) {
                        Ok(binding) => {
                            actions.push(UserAction::SetHotkey(capturing, Some(binding)));
                        }
                        Err(err) => {
                            actions.push(UserAction::RejectHotkey(err.message().to_owned()));
                        }
                    }
                }
                break;
            }
        }
    }

    for action in HotkeyAction::ALL {
        ui.horizontal(|ui| {
            ui.label(action.label());
            if hotkeys.capturing == Some(action) {
                ui.label("Pressione a combinação... (Esc cancela)");
                if ui.button("Cancelar").clicked() {
                    actions.push(UserAction::CancelHotkeyCapture);
                }
            } else {
                let current = hotkeys.binding(action).map(binding_label);
                ui.label(current.as_deref().unwrap_or("Nenhum"));
                if ui.button("Alterar").clicked() {
                    actions.push(UserAction::StartHotkeyCapture(action));
                }
                if ui
                    .add_enabled(current.is_some(), egui::Button::new("Remover"))
                    .clicked()
                {
                    actions.push(UserAction::SetHotkey(action, None));
                }
            }
        });
        if hotkeys.capturing == Some(action)
            && let Some(err) = &hotkeys.capture_error
        {
            ui.colored_label(ERROR_COLOR, err);
        }
        if hotkeys.failed.contains(&action) {
            ui.colored_label(
                ERROR_COLOR,
                "Não foi possível registrar: outro programa já usa esse atalho.",
            );
        }
    }
}

fn noise_gate(ui: &mut egui::Ui, audio: &AudioState, actions: &mut Vec<UserAction>) {
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

    ui.horizontal(|ui| {
        ui.label("Sensibilidade");
        let current = audio.noise_gate.unwrap_or(GATE_DEFAULT_DB);
        let mut value = current;
        let slider = ui.add_enabled(
            enabled,
            egui::Slider::new(&mut value, GATE_MIN_DB..=GATE_MAX_DB).suffix(" dB"),
        );
        if value != current {
            actions.push(UserAction::SetNoiseGate(Some(value)));
        }
        if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
            actions.push(UserAction::SaveAudioSettings);
        }
    });
    ui.small("Mais para a direita corta mais ruído, mas pode cortar fala baixa.");
    ui.add_space(4.0);

    match audio.mic_level {
        Some(MicLevelView {
            level_db,
            gate_open,
        }) => {
            let color = if gate_open { OPEN_COLOR } else { CLOSED_COLOR };
            ui.add(
                egui::ProgressBar::new(meter_fraction(level_db))
                    .desired_width(COMBO_WIDTH)
                    .fill(color)
                    .text(format!("{level_db} dB")),
            );
        }
        None => {
            ui.small("Entre em uma sala para ver o nível do microfone.");
        }
    }
}

fn meter_fraction(level_db: i8) -> f32 {
    ((f32::from(level_db) - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}

fn device_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    selected_text: &str,
    devices: &[AudioDeviceView],
    selected: Option<&str>,
    choose: fn(Option<String>) -> UserAction,
    actions: &mut Vec<UserAction>,
) {
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .width(COMBO_WIDTH)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(selected.is_none(), DEFAULT_DEVICE_LABEL)
                .clicked()
            {
                actions.push(choose(None));
            }
            for device in devices {
                let is_selected = selected == Some(device.id.as_str());
                if ui
                    .selectable_label(is_selected, device.name.as_str())
                    .clicked()
                {
                    actions.push(choose(Some(device.id.clone())));
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_maps_minus_70_to_0_db_onto_the_bar() {
        assert_eq!(meter_fraction(-100), 0.0);
        assert_eq!(meter_fraction(-70), 0.0);
        assert!((meter_fraction(-35) - 0.5).abs() < 1e-6);
        assert_eq!(meter_fraction(0), 1.0);
    }
}
