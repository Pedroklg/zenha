use eframe::egui::{self, RichText};

use crate::platform::hotkeys::{
    HotkeyAction, binding_from_key, binding_from_mouse_button, binding_label, binding_warning,
};
use crate::platform::settings::{NoiseMode, VoiceMode};
use zenha_protocol::RoomSummary;

use crate::state::{
    AdminState, AudioState, DEFAULT_DEVICE_LABEL, HotkeyState, MicLevelView, PersonalHookState,
    UserAction,
};
use crate::theme;
use crate::ui_model::AudioDeviceView;

mod admin;
mod gamers_club;
mod hotkeys;
mod mic;
mod webhooks;

use admin::admin_section;
use gamers_club::gamers_club_section;
use hotkeys::hotkeys_section;
use mic::noise_gate;

const WINDOW_WIDTH: f32 = 580.0;
const WINDOW_MARGIN_X: f32 = 48.0;
const WINDOW_MARGIN_Y: f32 = 120.0;
const SCROLLBAR_RESERVE: f32 = 24.0;
const MIN_WINDOW_WIDTH: f32 = 320.0;
const MIN_WINDOW_HEIGHT: f32 = 200.0;
const COMBO_WIDTH: f32 = 340.0;
const INVITE_INPUT_WIDTH: f32 = 220.0;
const SECTION_TOP_GAP: f32 = 14.0;
const SECTION_AFTER_SEP_GAP: f32 = 6.0;
const SECTION_TITLE_GAP: f32 = 10.0;
const GROUP_GAP: f32 = 14.0;
const ROW_GAP: f32 = 8.0;
const CONTROL_GAP: f32 = 4.0;
const HOTKEY_LABEL_WIDTH: f32 = 176.0;
const HOTKEY_BINDING_WIDTH: f32 = 96.0;
const GATE_LABEL_WIDTH: f32 = 132.0;
const GATE_CONTROL_WIDTH: f32 = 240.0;
const GATE_VALUE_WIDTH: f32 = 64.0;
const GATE_MIN_DB: i8 = -70;
const GATE_MAX_DB: i8 = -20;
const GATE_DEFAULT_DB: i8 = crate::platform::settings::DEFAULT_NOISE_GATE_DB;
const AVATAR_PREVIEW_SIZE: f32 = 48.0;
const METER_FLOOR_DB: f32 = -70.0;

pub struct SettingsView<'a> {
    pub server_url: &'a str,
    pub name_draft: &'a mut String,
    pub name_error: Option<String>,
    pub can_save_name: bool,
    pub in_room: bool,
    pub mic_muted: bool,
    pub audio: &'a AudioState,
    pub hotkeys: &'a HotkeyState,
    pub close_to_tray: bool,
    pub room_sounds: bool,
    pub muted_on_join: bool,
    pub invite_copied: bool,
    pub can_admin: bool,
    pub personal_hook: &'a PersonalHookState,
    pub admin: &'a mut AdminState,
    pub rooms: &'a [RoomSummary],
    pub avatar_identity: &'a str,
    pub avatar_version: Option<&'a str>,
    pub avatar_notice: Option<&'a str>,
}

pub fn show(ctx: &egui::Context, mut view: SettingsView<'_>) -> Vec<UserAction> {
    let mut actions = Vec::new();
    if !view.audio.open {
        return actions;
    }
    let mut open = true;
    let screen = ctx.content_rect().size();
    let max_height = (screen.y - WINDOW_MARGIN_Y).max(MIN_WINDOW_HEIGHT);
    let max_width = (screen.x - WINDOW_MARGIN_X).max(MIN_WINDOW_WIDTH);
    let content_width = WINDOW_WIDTH.min((max_width - SCROLLBAR_RESERVE).max(MIN_WINDOW_WIDTH));
    egui::Window::new("Configurações")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .max_width(max_width)
        .max_height(max_height)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .min_scrolled_height(max_height)
                .show(ui, |ui| {
                    ui.set_min_width(content_width);
                    ui.set_max_width(content_width);
                    content(ui, &mut view, &mut actions);
                });
        });
    if !open {
        actions.push(UserAction::CloseSettings);
    }
    actions
}

fn section_heading(ui: &mut egui::Ui, title: &str, with_separator: bool) {
    if with_separator {
        ui.add_space(SECTION_TOP_GAP);
        ui.separator();
        ui.add_space(SECTION_AFTER_SEP_GAP);
    }
    ui.heading(title);
    ui.add_space(SECTION_TITLE_GAP);
}

fn content(ui: &mut egui::Ui, view: &mut SettingsView<'_>, actions: &mut Vec<UserAction>) {
    let audio = view.audio;
    let hotkeys = view.hotkeys;

    section_heading(ui, "Áudio", false);

    ui.label("Microfone");
    ui.add_space(CONTROL_GAP);
    device_combo(
        ui,
        "input_device",
        &audio.input_label(),
        &audio.inputs,
        audio.input.as_deref(),
        UserAction::SelectInputDevice,
        actions,
    );
    ui.add_space(GROUP_GAP);

    ui.label("Saída de áudio");
    ui.add_space(CONTROL_GAP);
    device_combo(
        ui,
        "output_device",
        &audio.output_label(),
        &audio.outputs,
        audio.output.as_deref(),
        UserAction::SelectOutputDevice,
        actions,
    );
    ui.add_space(ROW_GAP);
    if ui.button("Atualizar lista").clicked() {
        actions.push(UserAction::RefreshAudioDevices);
    }
    ui.add_space(GROUP_GAP);

    ui.label("Modo de voz");
    ui.add_space(CONTROL_GAP);
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
    ui.add_space(GROUP_GAP);

    ui.label("Supressão de ruído");
    ui.add_space(CONTROL_GAP);
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
    match audio.noise {
        NoiseMode::Enhanced | NoiseMode::Strong => {
            ui.add_space(CONTROL_GAP);
            ui.small(if audio.noise == NoiseMode::Strong {
                "Intensa: a Reforçada com um filtro extra, para ambiente barulhento. Pode abafar um pouco a voz."
            } else {
                "Reforçada (padrão): melhor contra teclado e ventilador."
            });
            ui.add_space(ROW_GAP);
            let mut echo = audio.echo_cancellation;
            if ui
                .checkbox(&mut echo, "Cancelamento de eco")
                .on_hover_text(
                    "Ligue se você usa caixa de som e os outros ouvem a própria voz de volta. Com fone não precisa.",
                )
                .changed()
            {
                actions.push(UserAction::SetEchoCancellation(echo));
            }
            ui.add_space(ROW_GAP);
            noise_gate(ui, audio, view.in_room, view.mic_muted, actions);
        }
        NoiseMode::Standard => {
            ui.add_space(CONTROL_GAP);
            ui.small(
                "Reduzida: supressão leve com cancelamento de eco. Recomendada para caixa de som.",
            );
        }
        NoiseMode::Off => {}
    }

    section_heading(ui, "Atalhos globais", true);
    ui.small("Funcionam com o app minimizado e com um jogo em foco.");
    ui.add_space(ROW_GAP);
    hotkeys_section(ui, audio.voice_mode, hotkeys, actions);

    section_heading(ui, "Geral", true);
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
    ui.add_space(CONTROL_GAP);
    let mut muted_on_join = view.muted_on_join;
    if ui
        .checkbox(&mut muted_on_join, "Entrar com o microfone desligado")
        .changed()
    {
        actions.push(UserAction::SetMutedOnJoin(muted_on_join));
    }
    ui.add_space(CONTROL_GAP);
    let mut room_sounds = view.room_sounds;
    if ui
        .checkbox(&mut room_sounds, "Sons de entrada e saída")
        .changed()
    {
        actions.push(UserAction::SetRoomSounds(room_sounds));
    }

    section_heading(ui, "Servidor", true);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Servidor:").color(theme::TEXT_SECONDARY));
        ui.label(view.server_url.trim());
    });
    ui.add_space(CONTROL_GAP);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Nome:").color(theme::TEXT_SECONDARY));
        let response = ui.add(egui::TextEdit::singleline(view.name_draft).desired_width(200.0));
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let save = ui.add_enabled(view.can_save_name, egui::Button::new("Salvar nome"));
        if view.can_save_name && (save.clicked() || enter) {
            actions.push(UserAction::SaveDisplayName);
        }
    });
    if let Some(error) = &view.name_error {
        ui.small(RichText::new(error).color(theme::ERROR));
    }
    ui.add_space(ROW_GAP);
    avatar_row(ui, view, actions);
    ui.add_space(ROW_GAP);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = ROW_GAP;
        if ui.button("Copiar link de convite").clicked() {
            actions.push(UserAction::CopyInviteLink);
        }
        if ui.button("Trocar servidor").clicked() {
            actions.push(UserAction::ChangeServer);
        }
    });
    if view.invite_copied {
        ui.add_space(CONTROL_GAP);
        ui.small(RichText::new("Link copiado.").color(theme::ACCENT_TEXT));
    }

    gamers_club_section(ui, view.can_admin, view.personal_hook, actions);
    admin_section(ui, view.can_admin, view.admin, view.rooms, actions);
}

fn can_remove_avatar(version: Option<&str>) -> bool {
    version.is_some()
}

fn avatar_row(ui: &mut egui::Ui, view: &SettingsView<'_>, actions: &mut Vec<UserAction>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = ROW_GAP;
        ui.label(RichText::new("Foto:").color(theme::TEXT_SECONDARY));
        theme::avatar(
            ui,
            AVATAR_PREVIEW_SIZE,
            view.avatar_identity,
            view.name_draft.as_str(),
            view.avatar_version,
            true,
        );
        if ui.button("Escolher foto").clicked() {
            actions.push(UserAction::PickAvatar);
        }
        let remove = ui.add_enabled(
            can_remove_avatar(view.avatar_version),
            egui::Button::new("Remover foto"),
        );
        if remove.clicked() {
            actions.push(UserAction::RemoveAvatar);
        }
    });
    if let Some(notice) = view.avatar_notice {
        ui.small(RichText::new(notice).color(theme::ERROR));
    }
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
    use super::mic::{MicMeterView, meter_fraction, mic_meter_view};
    use super::*;

    #[test]
    fn remove_avatar_is_enabled_only_with_a_photo() {
        assert!(!can_remove_avatar(None));
        assert!(can_remove_avatar(Some("0123456789abcdef")));
    }

    #[test]
    fn meter_maps_minus_70_to_0_db_onto_the_bar() {
        assert_eq!(meter_fraction(-100), 0.0);
        assert_eq!(meter_fraction(-70), 0.0);
        assert!((meter_fraction(-35) - 0.5).abs() < 1e-6);
        assert_eq!(meter_fraction(0), 1.0);
    }

    #[test]
    fn mic_meter_view_selects_message_or_level_by_room_and_mute_state() {
        let level = MicLevelView {
            level_db: -35,
            gate_open: true,
        };
        assert_eq!(
            mic_meter_view(false, false, None),
            MicMeterView::Message("Entre em uma sala para ver o nível do microfone.")
        );
        assert_eq!(
            mic_meter_view(false, true, Some(level)),
            MicMeterView::Message("Entre em uma sala para ver o nível do microfone.")
        );
        assert_eq!(
            mic_meter_view(true, true, None),
            MicMeterView::Message("Microfone desligado.")
        );
        assert_eq!(
            mic_meter_view(true, true, Some(level)),
            MicMeterView::Message("Microfone desligado.")
        );
        assert_eq!(
            mic_meter_view(true, false, None),
            MicMeterView::Message("Aguardando sinal do microfone…")
        );
        assert_eq!(
            mic_meter_view(true, false, Some(level)),
            MicMeterView::Level(level)
        );
    }
}
