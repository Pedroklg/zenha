//! Gamers Club personal webhook section of the settings, visible to all users.

use eframe::egui::{self, RichText};

use super::{CONTROL_GAP, ROW_GAP, section_heading};
use crate::state::{PersonalHookPhase, PersonalHookState, UserAction};
use crate::theme;

const STEPS: [&str; 4] = [
    "1. Abra a extensão GamersClub Booster no navegador.",
    "2. Vá na aba \"Discord\".",
    "3. Cole a URL no campo do webhook e, em \"Nome do servidor\", escreva Zenha.",
    "4. Clique em \"Testar\": o cartão aparece no chat da sala em que você estiver no Zenha.",
];
const ROOM_NOTICE: &str =
    "O cartão só aparece na sala em que você estiver. Se reinstalar o Zenha, copie a URL de novo.";
const COPY_BUTTON_RESERVE: f32 = 88.0;
const MIN_URL_FIELD_WIDTH: f32 = 200.0;

pub(super) fn gamers_club_section(
    ui: &mut egui::Ui,
    connected: bool,
    personal_hook: &PersonalHookState,
    actions: &mut Vec<UserAction>,
) {
    section_heading(ui, "Gamers Club", true);

    if !connected {
        ui.weak("Entre em um servidor para ver sua URL pessoal do Gamers Club.");
        return;
    }

    match &personal_hook.phase {
        PersonalHookPhase::Idle | PersonalHookPhase::Loading => {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = ROW_GAP;
                ui.spinner();
                ui.label("Carregando…");
            });
        }
        PersonalHookPhase::Ready(url) => {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = ROW_GAP;
                let width = (ui.available_width() - COPY_BUTTON_RESERVE).max(MIN_URL_FIELD_WIDTH);
                let mut url_text = url.0.as_str();
                ui.add(
                    egui::TextEdit::singleline(&mut url_text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(width),
                );
                if ui.add(theme::accent_button("Copiar")).clicked() {
                    actions.push(UserAction::CopyPersonalHook);
                }
            });
            if personal_hook.copied {
                ui.add_space(CONTROL_GAP);
                ui.small(RichText::new("URL copiada.").color(theme::ACCENT_TEXT));
            }
        }
        PersonalHookPhase::Failed(message) => {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = ROW_GAP;
                ui.colored_label(theme::ERROR, message);
                if ui.button("Tentar de novo").clicked() {
                    actions.push(UserAction::RetryPersonalHook);
                }
            });
        }
    }

    ui.add_space(ROW_GAP);
    for step in STEPS {
        ui.small(step);
    }
    ui.add_space(CONTROL_GAP);
    ui.small(RichText::new(ROOM_NOTICE).color(theme::TEXT_WEAK));
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::state::SecretText;

    const URL: &str = "https://voz.exemplo/hooks/u/client-1/0123456789abcdef0123456789abcdef";

    struct View {
        connected: bool,
        personal_hook: PersonalHookState,
        actions: Vec<UserAction>,
    }

    fn harness(connected: bool, personal_hook: PersonalHookState) -> Harness<'static, View> {
        Harness::builder()
            .with_size(egui::Vec2::new(600.0, 400.0))
            .with_accessibility_check(false)
            .with_context_setup(theme::install)
            .build_ui_state(
                |ui, view: &mut View| {
                    gamers_club_section(ui, view.connected, &view.personal_hook, &mut view.actions);
                },
                View {
                    connected,
                    personal_hook,
                    actions: Vec::new(),
                },
            )
    }

    #[test]
    fn loading_state_shows_label_steps_and_notice() {
        let mut state = PersonalHookState::default();
        state.phase = PersonalHookPhase::Loading;
        let mut harness = harness(true, state);
        harness.run_steps(2);

        harness.get_by_label("Gamers Club");
        harness.get_by_label("Carregando…");
        harness.get_by_label(STEPS[0]);
        harness.get_by_label(ROOM_NOTICE);
    }

    #[test]
    fn ready_state_shows_readonly_url_and_copy_emits_action() {
        let mut state = PersonalHookState::default();
        state.phase = PersonalHookPhase::Ready(SecretText(URL.to_owned()));
        state.copied = true;
        let mut harness = harness(true, state);
        harness.run_steps(2);

        harness.get_by_label("URL copiada.");
        harness.get_by_label("Copiar").click();
        harness.run_steps(2);
        assert_eq!(harness.state().actions, vec![UserAction::CopyPersonalHook]);
    }

    #[test]
    fn error_state_shows_message_and_retry_button() {
        let mut state = PersonalHookState::default();
        state.phase =
            PersonalHookPhase::Failed("Não foi possível falar com o servidor.".to_owned());
        let mut harness = harness(true, state);
        harness.run_steps(2);

        harness.get_by_label("Não foi possível falar com o servidor.");
        harness.get_by_label("Tentar de novo").click();
        harness.run_steps(2);
        assert_eq!(harness.state().actions, vec![UserAction::RetryPersonalHook]);
    }
}
