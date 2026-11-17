//! Global hotkeys section of the settings.

use super::*;

pub(super) fn hotkeys_section(
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

    for (idx, action) in HotkeyAction::ALL.into_iter().enumerate() {
        if idx > 0 {
            ui.add_space(CONTROL_GAP);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = ROW_GAP;
            ui.allocate_ui_with_layout(
                egui::vec2(HOTKEY_LABEL_WIDTH, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(HOTKEY_LABEL_WIDTH);
                    ui.label(action.label());
                },
            );
            if hotkeys.capturing == Some(action) {
                ui.label("Pressione a combinação... (Esc cancela)");
                if ui.button("Cancelar").clicked() {
                    actions.push(UserAction::CancelHotkeyCapture);
                }
            } else {
                let current = hotkeys.binding(action).map(binding_label);
                ui.allocate_ui_with_layout(
                    egui::vec2(HOTKEY_BINDING_WIDTH, ui.spacing().interact_size.y),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(HOTKEY_BINDING_WIDTH);
                        ui.label(current.as_deref().unwrap_or("Nenhum"));
                    },
                );
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
                ui.add_space(CONTROL_GAP);
                ui.colored_label(
                    theme::ERROR,
                    "Defina a tecla de push-to-talk; sem ela o microfone fica fechado.",
                );
            }
        }
        if let Some(warning) = hotkeys.binding(action).and_then(binding_warning) {
            ui.add_space(CONTROL_GAP);
            ui.small(RichText::new(warning).color(theme::ACCENT));
        }
        if hotkeys.capturing == Some(action)
            && let Some(err) = &hotkeys.capture_error
        {
            ui.add_space(CONTROL_GAP);
            ui.colored_label(theme::ERROR, err);
        }
        if hotkeys.failed.contains(&action) {
            ui.add_space(CONTROL_GAP);
            ui.colored_label(
                theme::ERROR,
                "Não foi possível registrar: outro programa já usa esse atalho.",
            );
        }
    }
}
