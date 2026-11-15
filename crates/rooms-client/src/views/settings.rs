use eframe::egui::{self, RichText};

use crate::hotkeys::{HotkeyAction, binding_from_key, binding_label, binding_warning};
use crate::settings::{NoiseMode, VoiceMode};
use crate::state::{AudioState, DEFAULT_DEVICE_LABEL, HotkeyState, MicLevelView, UserAction};
use crate::theme;
use crate::ui_model::AudioDeviceView;

const COMBO_WIDTH: f32 = 320.0;
const GATE_MIN_DB: i8 = -70;
const GATE_MAX_DB: i8 = -20;
const GATE_DEFAULT_DB: i8 = -45;
const METER_FLOOR_DB: f32 = -70.0;

pub struct SettingsView<'a> {
    pub server_url: &'a str,
    pub display_name: &'a str,
    pub audio: &'a AudioState,
    pub hotkeys: &'a HotkeyState,
    pub close_to_tray: bool,
    pub muted_on_join: bool,
    pub invite_copied: bool,
}

pub fn show(ctx: &egui::Context, view: SettingsView<'_>) -> Vec<UserAction> {
    let mut actions = Vec::new();
    if !view.audio.open {
        return actions;
    }
    let mut open = true;
    let max_height = (ctx.content_rect().height() - 120.0).max(200.0);
    egui::Window::new("Configurações")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .max_height(max_height)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .show(ui, |ui| content(ui, &view, &mut actions));
        });
    if !open {
        actions.push(UserAction::CloseSettings);
    }
    actions
}

fn content(ui: &mut egui::Ui, view: &SettingsView<'_>, actions: &mut Vec<UserAction>) {
    let audio = view.audio;
    let hotkeys = view.hotkeys;
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
    ui.add_space(8.0);

    ui.label("Modo de voz");
    egui::ComboBox::from_id_salt("voice_mode")
        .selected_text(audio.voice_mode.label())
        .width(COMBO_WIDTH)
        .show_ui(ui, |ui| {
            for mode in VoiceMode::ALL {
                if ui
                    .selectable_label(audio.voice_mode == mode, mode.label())
                    .clicked()
                {
                    actions.push(UserAction::SelectVoiceMode(mode));
                }
            }
        });

    ui.separator();
    ui.heading("Atalhos globais");
    ui.small("Funcionam com o app minimizado e com um jogo em foco.");
    hotkeys_section(ui, audio.voice_mode, hotkeys, actions);

    ui.separator();
    ui.heading("Geral");
    let mut close = view.close_to_tray;
    if ui
        .checkbox(
            &mut close,
            "Fechar para a bandeja (o app continua na chamada)",
        )
        .changed()
    {
        actions.push(UserAction::SetCloseToTray(close));
    }
    let mut muted_on_join = view.muted_on_join;
    if ui
        .checkbox(&mut muted_on_join, "Entrar com o microfone mudo")
        .changed()
    {
        actions.push(UserAction::SetMutedOnJoin(muted_on_join));
    }

    ui.separator();
    ui.heading("Servidor");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Servidor:").color(theme::TEXT_SECONDARY));
        ui.label(view.server_url.trim());
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Nome:").color(theme::TEXT_SECONDARY));
        ui.label(view.display_name.trim());
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Copiar link de convite").clicked() {
            actions.push(UserAction::CopyInviteLink);
        }
        if ui.button("Trocar servidor ou nome").clicked() {
            actions.push(UserAction::ChangeServer);
        }
    });
    if view.invite_copied {
        ui.small(RichText::new("Link copiado.").color(theme::ACCENT_TEXT));
    }
}

fn hotkeys_section(
    ui: &mut egui::Ui,
    voice_mode: VoiceMode,
    hotkeys: &HotkeyState,
    actions: &mut Vec<UserAction>,
) {
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
        if action == HotkeyAction::PushToTalk {
            ui.small("Usado só no modo Push-to-talk.");
            if voice_mode == VoiceMode::PushToTalk && hotkeys.ptt.is_none() {
                ui.colored_label(
                    theme::ERROR,
                    "Defina a tecla de push-to-talk; sem ela o microfone fica fechado.",
                );
            }
        }
        if let Some(warning) = hotkeys.binding(action).and_then(binding_warning) {
            ui.small(RichText::new(warning).color(theme::ACCENT));
        }
        if hotkeys.capturing == Some(action)
            && let Some(err) = &hotkeys.capture_error
        {
            ui.colored_label(theme::ERROR, err);
        }
        if hotkeys.failed.contains(&action) {
            ui.colored_label(
                theme::ERROR,
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
            let color = if gate_open {
                theme::SPEAKING
            } else {
                theme::INACTIVE_DOT
            };
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
