use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};

use crate::state::{LoginForm, OnboardingState, UserAction};
use crate::theme;

const CARD_WIDTH: f32 = 420.0;

pub fn show(
    ui: &mut egui::Ui,
    form: &mut LoginForm,
    onboarding: &mut OnboardingState,
    show_password: bool,
) -> Vec<UserAction> {
    let mut actions = Vec::new();
    let mut submitted = false;

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::BG))
        .show(ui, |ui| {
            let errors = form.errors();
            let valid = errors.is_empty();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let top_pad = (ui.available_height() - 440.0).max(32.0) * 0.42;
                    ui.add_space(top_pad);

                    ui.horizontal(|ui| {
                        let side_pad = ((ui.available_width() - CARD_WIDTH) * 0.5).max(16.0);
                        ui.add_space(side_pad);

                        let width = (ui.available_width() - side_pad).min(CARD_WIDTH);
                        egui::Frame::new()
                            .fill(theme::PANEL)
                            .stroke(Stroke::new(1.0, theme::BORDER))
                            .corner_radius(CornerRadius::same(14))
                            .inner_margin(Margin::same(36))
                            .show(ui, |ui| {
                                ui.set_width((width - 72.0).max(200.0));
                                ui.vertical(|ui| {
                                    submitted = card_contents(
                                        ui,
                                        form,
                                        onboarding,
                                        &errors.display_name,
                                        &errors.server_url,
                                        show_password,
                                        valid,
                                        &mut actions,
                                    );
                                });
                            });
                    });
                    ui.add_space(24.0);
                });
        });

    if submitted && form.is_valid() {
        actions.push(UserAction::SubmitOnboarding);
    }

    actions
}

#[expect(
    clippy::too_many_arguments,
    reason = "egui view renders form inputs and callback state directly"
)]
fn card_contents(
    ui: &mut egui::Ui,
    form: &mut LoginForm,
    onboarding: &mut OnboardingState,
    name_error: &Option<String>,
    server_error: &Option<String>,
    show_password: bool,
    valid: bool,
    actions: &mut Vec<UserAction>,
) -> bool {
    let mut submitted = false;
    let width = ui.available_width();

    ui.label(
        RichText::new("Zenha")
            .font(theme::display(30.0))
            .color(theme::TEXT),
    );
    ui.add_space(20.0);

    field_label(ui, "Como você quer ser chamado");
    let name_resp = ui.add(
        egui::TextEdit::singleline(&mut form.display_name)
            .desired_width(width)
            .margin(Margin::symmetric(14, 10)),
    );
    submitted |= enter_pressed(ui, &name_resp);
    if !form.display_name.is_empty()
        && let Some(err) = name_error
    {
        ui.small(RichText::new(err).color(theme::ERROR));
    }
    ui.add_space(12.0);

    field_label(ui, "Endereço do servidor");
    let server_resp = ui.add(
        egui::TextEdit::singleline(&mut form.server_url)
            .hint_text("https://voz.exemplo")
            .desired_width(width)
            .margin(Margin::symmetric(14, 10)),
    );
    submitted |= enter_pressed(ui, &server_resp);
    if !form.server_url.is_empty()
        && let Some(err) = server_error
    {
        ui.small(RichText::new(err).color(theme::ERROR));
    }

    if show_password {
        ui.add_space(12.0);
        if onboarding.password_rejected {
            egui::Frame::new()
                .fill(theme::ERROR_BG)
                .stroke(Stroke::new(1.0, theme::ERROR_BORDER))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width((width - 24.0).max(120.0));
                    ui.label(
                        RichText::new("Senha recusada pelo servidor.")
                            .size(13.0)
                            .color(theme::ERROR),
                    );
                });
            ui.add_space(8.0);
        }
        field_label(ui, "Senha do servidor");
        let pwd_resp = ui.add(
            egui::TextEdit::singleline(&mut form.invite_code)
                .password(true)
                .desired_width(width)
                .margin(Margin::symmetric(14, 10)),
        );
        submitted |= enter_pressed(ui, &pwd_resp);
    }

    ui.add_space(20.0);
    let enter_btn = theme::accent_button("Entrar").min_size(Vec2::new(width, 42.0));
    if ui.add_enabled(valid, enter_btn).clicked() {
        submitted = true;
    }

    ui.add_space(10.0);
    let paste_btn = egui::Button::new(
        RichText::new("Colar um link de convite")
            .font(theme::semibold(13.0))
            .color(theme::TEXT_SECONDARY),
    )
    .fill(Color32::TRANSPARENT)
    .stroke(Stroke::new(1.0, theme::BORDER))
    .corner_radius(CornerRadius::same(8))
    .min_size(Vec2::new(width, 38.0));
    if ui.add(paste_btn).clicked() {
        actions.push(UserAction::ToggleInviteLink);
    }

    if onboarding.link_open {
        ui.add_space(8.0);
        let link_resp = ui.add(
            egui::TextEdit::singleline(&mut onboarding.link_input)
                .hint_text("zenha://entrar?servidor=...&senha=...")
                .desired_width(width)
                .margin(Margin::symmetric(12, 8)),
        );
        if link_resp.changed() {
            actions.push(UserAction::PasteInviteLink(onboarding.link_input.clone()));
        }
        if let Some(err) = &onboarding.link_error {
            ui.small(RichText::new(err).color(theme::ERROR));
        }
    }

    submitted
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(13.0).color(theme::TEXT_SECONDARY));
    ui.add_space(4.0);
}

fn enter_pressed(ui: &egui::Ui, response: &egui::Response) -> bool {
    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}
