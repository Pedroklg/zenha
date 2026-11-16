//! Telas de lobby, sala ativa, grade de participantes e diálogos administrativos.

use std::collections::BTreeMap;

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Vec2,
};
use rooms_protocol::{MAX_ROOM_TITLE_LEN, RoomSummary};

use super::chat;
use super::participant_menu::{
    ParticipantMenuCtx, VolumeMenuScope, participant_volume_badge, participant_volume_menu,
};
use super::room_watch::{ShareRenderers, local_share_preview, watching_center};
use super::share::VideoRenderer;
use super::sidebar::{SidebarArgs, SidebarUser, sidebar};
use crate::chat_images::ChatImages;
use crate::icons;
use crate::state::{
    AdminDialog, LobbyVoiceState, LocalShare, RoomState, UpdateBanner, UserAction,
    participant_volume_for, room_display_title,
};
use crate::theme;
use crate::ui_model::ParticipantView;

pub(super) const HEADER_HEIGHT: f32 = 64.0;
const FOOTER_HEIGHT: f32 = 72.0;
const TWO_COL_BREAKPOINT: f32 = 520.0;

#[expect(
    clippy::too_many_arguments,
    reason = "egui room view takes panel state and control handles directly"
)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut RoomState,
    rooms: &[RoomSummary],
    server_host: &str,
    renderer: &mut VideoRenderer,
    preview_renderer: &mut VideoRenderer,
    images: &mut ChatImages,
    stream_volume: u8,
    participant_volumes: &BTreeMap<String, u16>,
    ptt_hint: Option<&str>,
    update_banner: Option<&UpdateBanner>,
    is_admin: bool,
) -> Vec<UserAction> {
    let mut actions = Vec::new();
    if state.share.preview.is_none() {
        preview_renderer.reset();
    }
    if state.share.watching.is_none() {
        renderer.reset();
    }

    if let Some(watch) = state.share.watching.as_ref().filter(|w| w.fullscreen) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::VIDEO_BG))
            .show(ui, |ui| {
                renderer.show(ui, watch);
                let button = egui::Area::new(egui::Id::unique("exit_fullscreen"))
                    .anchor(egui::Align2::RIGHT_TOP, [-12.0, 12.0])
                    .show(ui.ctx(), |ui| ui.button("Sair da tela cheia (Esc)"));
                if button.inner.clicked() {
                    actions.push(UserAction::ToggleFullscreen);
                }
            });
        return actions;
    }

    let local_name = local_display_name(&state.participants, &state.local_identity);
    let user = SidebarUser {
        name: local_name,
        connection: Some(state.status),
        ptt_hint,
        muted: state.muted,
        deafened: state.deafened,
    };
    sidebar(
        ui,
        SidebarArgs {
            user,
            current_room: Some(&state.room),
            local_identity: &state.local_identity,
            participants: &state.participants,
            share: Some(&state.share),
            participant_volumes,
            rooms,
            server_host,
            update_banner,
            is_admin,
        },
        &mut actions,
    );

    if state.chat.open {
        actions.extend(chat::show(
            ui,
            &mut state.chat,
            &state.participants,
            &state.local_identity,
            is_admin,
            images,
        ));
    }

    if let Some(watch) = state.share.watching.clone() {
        watching_center(
            ui,
            state,
            rooms,
            &watch,
            ShareRenderers {
                watch: renderer,
                preview: preview_renderer,
            },
            stream_volume,
            participant_volumes,
            is_admin,
            &mut actions,
        );
    } else {
        main_center(
            ui,
            state,
            rooms,
            preview_renderer,
            participant_volumes,
            is_admin,
            &mut actions,
        );
    }

    actions
}

#[expect(
    clippy::too_many_arguments,
    reason = "egui view takes room state, admin context and actions directly"
)]
pub fn show_lobby(
    ui: &mut egui::Ui,
    display_name: &str,
    voice: LobbyVoiceState,
    rooms: &[RoomSummary],
    server_host: &str,
    error: Option<&str>,
    notice: Option<&str>,
    update_banner: Option<&UpdateBanner>,
    is_admin: bool,
) -> Vec<UserAction> {
    let mut actions = Vec::new();
    let user = SidebarUser {
        name: display_name.trim(),
        connection: None,
        ptt_hint: None,
        muted: voice.muted,
        deafened: voice.deafened,
    };
    let empty_volumes = BTreeMap::new();
    sidebar(
        ui,
        SidebarArgs {
            user,
            current_room: None,
            local_identity: "",
            participants: &[],
            share: None,
            participant_volumes: &empty_volumes,
            rooms,
            server_host,
            update_banner,
            is_admin,
        },
        &mut actions,
    );

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(Margin::same(24)),
        )
        .show(ui, |ui| {
            if let Some(message) = error {
                egui::Frame::new()
                    .fill(theme::ERROR_BG)
                    .stroke(Stroke::new(1.0, theme::ERROR_BORDER))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.label(RichText::new(message).size(13.0).color(theme::ERROR));
                    });
                ui.add_space(16.0);
            }
            if let Some(message) = notice {
                dismissible_banner(
                    ui,
                    message,
                    theme::ERROR,
                    theme::ERROR_BG,
                    theme::ERROR_BORDER,
                    || UserAction::DismissLobbyNotice,
                    &mut actions,
                );
            }
            ui.allocate_ui_with_layout(
                ui.available_size(),
                Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    ui.label(
                        RichText::new("Escolha uma sala para entrar.")
                            .size(15.0)
                            .color(theme::TEXT_SECONDARY),
                    );
                },
            );
        });

    actions
}

fn main_center(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    preview_renderer: &mut VideoRenderer,
    participant_volumes: &BTreeMap<String, u16>,
    is_admin: bool,
    actions: &mut Vec<UserAction>,
) {
    let title = room_display_title(rooms, &state.room);
    egui::Panel::top("room_header")
        .exact_size(HEADER_HEIGHT)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(24, 0)),
        )
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.allocate_ui_with_layout(rect.size(), Layout::right_to_left(Align::Center), |ui| {
                if chat_toggle_button(ui, state.chat.open, state.chat.unread) {
                    actions.push(UserAction::ToggleChat);
                }
                ui.add_space(8.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(title)
                                .font(theme::display(20.0))
                                .color(theme::TEXT),
                        )
                        .truncate(),
                    );
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(people_count_label(state.participants.len()))
                                .size(13.0)
                                .color(theme::TEXT_WEAK),
                        )
                        .truncate(),
                    );
                });
            });
            let full = ui.clip_rect();
            ui.painter().hline(
                full.x_range(),
                rect.bottom(),
                Stroke::new(1.0, theme::BORDER),
            );
        });

    egui::Panel::bottom("room_footer")
        .exact_size(FOOTER_HEIGHT)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::symmetric(24, 0)),
        )
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let full = ui.clip_rect();
            ui.painter()
                .hline(full.x_range(), rect.top(), Stroke::new(1.0, theme::BORDER));
            ui.allocate_ui_with_layout(rect.size(), Layout::left_to_right(Align::Center), |ui| {
                match state.share.local {
                    LocalShare::Idle => {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Transmitir tela").font(theme::semibold(14.0)),
                                )
                                .min_size(Vec2::new(130.0, 40.0)),
                            )
                            .clicked()
                        {
                            actions.push(UserAction::OpenSharePicker);
                        }
                    }
                    LocalShare::Starting => {
                        ui.add_enabled(
                            false,
                            egui::Button::new("Iniciando transmissão...")
                                .min_size(Vec2::new(160.0, 40.0)),
                        );
                    }
                    LocalShare::Active => {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Parar transmissão").font(theme::semibold(14.0)),
                                )
                                .min_size(Vec2::new(140.0, 40.0)),
                            )
                            .clicked()
                        {
                            actions.push(UserAction::StopShare);
                        }
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let leave_btn = egui::Button::new(
                        RichText::new("Sair da sala")
                            .font(theme::semibold(14.0))
                            .color(theme::ERROR),
                    )
                    .fill(theme::ERROR_BG)
                    .stroke(Stroke::new(1.0, theme::ERROR_BORDER))
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(116.0, 40.0));
                    if ui.add(leave_btn).clicked() {
                        actions.push(UserAction::Leave);
                    }
                });
            });
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(Margin::same(24)),
        )
        .show(ui, |ui| {
            share_error_banner(ui, state.share.error.as_deref(), actions);
            collapsed_chat_notice_banner(ui, state, actions);
            local_share_preview(ui, &state.share, preview_renderer, false, actions);
            participants_grid(ui, state, rooms, participant_volumes, is_admin, actions);
        });
}

pub(super) fn share_error_banner(
    ui: &mut egui::Ui,
    error: Option<&str>,
    actions: &mut Vec<UserAction>,
) {
    if let Some(error) = error {
        dismissible_banner(
            ui,
            error,
            theme::ERROR,
            theme::ERROR_BG,
            theme::ERROR_BORDER,
            || UserAction::DismissShareError,
            actions,
        );
    }
}

pub(super) fn collapsed_chat_notice_banner(
    ui: &mut egui::Ui,
    state: &RoomState,
    actions: &mut Vec<UserAction>,
) {
    if state.chat.open {
        return;
    }
    if let Some(message) = state.chat.notice.as_deref() {
        dismissible_banner(
            ui,
            message,
            theme::TEXT,
            theme::PANEL_DARK,
            theme::BORDER,
            || UserAction::DismissChatNotice,
            actions,
        );
    }
}

fn dismissible_banner(
    ui: &mut egui::Ui,
    message: &str,
    text_color: Color32,
    fill: Color32,
    border: Color32,
    dismiss_action: impl FnOnce() -> UserAction,
    actions: &mut Vec<UserAction>,
) {
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(text_color, message);
                if ui.small_button("Fechar").clicked() {
                    actions.push(dismiss_action());
                }
            });
        });
    ui.add_space(12.0);
}

fn participants_grid(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    participant_volumes: &BTreeMap<String, u16>,
    is_admin: bool,
    actions: &mut Vec<UserAction>,
) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let avail = ui.available_width();
            let gap = 14.0;
            let cols = if avail < TWO_COL_BREAKPOINT { 1 } else { 2 };
            let card_width = if cols == 2 {
                ((avail - gap) * 0.5).max(180.0)
            } else {
                avail.max(180.0)
            };

            for pair in state.participants.chunks(cols) {
                ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for participant in pair {
                        let volume =
                            participant_volume_for(participant_volumes, &participant.identity);
                        ui.allocate_ui(Vec2::new(card_width, 84.0), |ui| {
                            participant_card(
                                ui,
                                state,
                                rooms,
                                participant,
                                volume,
                                card_width,
                                is_admin,
                                actions,
                            );
                        });
                    }
                });
                ui.add_space(gap);
            }
        });
}

#[expect(
    clippy::too_many_arguments,
    reason = "egui view takes room state, admin context and actions directly"
)]
fn participant_card(
    ui: &mut egui::Ui,
    state: &RoomState,
    rooms: &[RoomSummary],
    participant: &ParticipantView,
    volume: u16,
    width: f32,
    is_admin: bool,
    actions: &mut Vec<UserAction>,
) {
    let is_self = !state.local_identity.is_empty() && participant.identity == state.local_identity;
    let is_sharing_remote = state.share.is_sharing_remote(&participant.identity);
    let is_sharing_local = is_self && state.share.local == LocalShare::Active;
    let border_color = if participant.speaking {
        theme::SPEAKING
    } else {
        theme::PANEL
    };

    let card_response = egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(2.0, border_color))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            let inner_width = (width - 36.0).max(80.0);
            ui.set_min_width(inner_width);
            ui.set_max_width(inner_width);
            ui.allocate_ui_with_layout(
                Vec2::new(inner_width, 52.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    theme::avatar(
                        ui,
                        52.0,
                        &participant.name,
                        theme::avatar_color(&participant.identity, is_self),
                    );
                    ui.add_space(8.0);

                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), 52.0),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            if is_sharing_remote
                                && ui.add(theme::accent_button("Assistir")).clicked()
                            {
                                actions.push(UserAction::Watch(participant.identity.clone()));
                            }

                            let text_col_w = ui.available_width().max(40.0);
                            ui.allocate_ui_with_layout(
                                Vec2::new(text_col_w, 48.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_max_width(text_col_w);
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    ui.spacing_mut().interact_size.y = 0.0;
                                    participant_name_row(ui, participant, is_self, volume);
                                    participant_status_row(
                                        ui,
                                        participant,
                                        is_sharing_remote || is_sharing_local,
                                    );
                                },
                            );
                        },
                    );
                },
            );
        })
        .response;

    if !is_self {
        participant_volume_menu(
            &card_response,
            ParticipantMenuCtx {
                scope: VolumeMenuScope::Grid,
                identity: &participant.identity,
                current_volume: volume,
                is_admin,
                is_sharing: is_sharing_remote,
                current_room: &state.room,
                rooms,
            },
            actions,
        );
    }
}

fn participant_name_row(
    ui: &mut egui::Ui,
    participant: &ParticipantView,
    is_self: bool,
    volume: u16,
) {
    let badge = if is_self {
        Some(("(você)".to_owned(), 13.0))
    } else {
        participant_volume_badge(volume, is_self).map(|label| (label, 12.0))
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let max_name_w = if badge.is_some() {
            (ui.available_width() - 48.0).max(24.0)
        } else {
            ui.available_width().max(24.0)
        };
        ui.scope(|ui| {
            ui.set_max_width(max_name_w);
            ui.add(
                egui::Label::new(
                    RichText::new(&participant.name)
                        .font(theme::semibold(15.0))
                        .color(theme::TEXT),
                )
                .truncate(),
            );
        });
        if let Some((label, size)) = badge {
            ui.label(RichText::new(label).size(size).color(theme::TEXT_WEAK));
        }
    });
}

fn participant_status_row(ui: &mut egui::Ui, participant: &ParticipantView, is_sharing: bool) {
    if is_sharing {
        ui.add(
            egui::Label::new(
                RichText::new("Transmitindo")
                    .size(13.0)
                    .color(theme::TEXT_WEAK),
            )
            .truncate(),
        );
    } else if participant.speaking {
        ui.add(
            egui::Label::new(
                RichText::new("Falando")
                    .font(theme::semibold(13.0))
                    .color(theme::SPEAKING),
            )
            .truncate(),
        );
    } else if participant.muted {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            icons::mic(ui.painter(), icon_rect, theme::TEXT_WEAK, true);
            ui.add(
                egui::Label::new(
                    RichText::new("Microfone desligado")
                        .size(13.0)
                        .color(theme::TEXT_WEAK),
                )
                .truncate(),
            );
        });
    } else {
        ui.add(
            egui::Label::new(RichText::new("Ouvindo").size(13.0).color(theme::TEXT_WEAK))
                .truncate(),
        );
    }
}

pub fn admin_dialog(ctx: &egui::Context, dialog: &mut Option<AdminDialog>) -> Vec<UserAction> {
    let Some(current) = dialog.as_mut() else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    let frame = egui::Frame::window(&ctx.global_style())
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(18));

    let modal = egui::Modal::new(egui::Id::unique("admin_dialog"))
        .backdrop_color(egui::Color32::from_black_alpha(180))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.set_max_width(400.0);
            match current {
                AdminDialog::CreateRoom { title, error } => {
                    room_title_dialog_ui(
                        ui,
                        "Nova sala",
                        "Criar",
                        title,
                        error.as_deref(),
                        &mut actions,
                    );
                }
                AdminDialog::RenameRoom { title, error, .. } => {
                    room_title_dialog_ui(
                        ui,
                        "Renomear sala",
                        "Salvar",
                        title,
                        error.as_deref(),
                        &mut actions,
                    );
                }
                AdminDialog::DeleteRoom { title, .. } => {
                    confirm_danger_dialog_ui(
                        ui,
                        "Apagar sala",
                        &format!(
                            "Apagar a sala \"{title}\"? Todos os participantes serão removidos dela."
                        ),
                        "Apagar",
                        &mut actions,
                    );
                }
                AdminDialog::KickParticipant { name, .. } => {
                    confirm_danger_dialog_ui(
                        ui,
                        "Remover participante",
                        &format!("Remover \"{name}\" da sala atual?"),
                        "Remover",
                        &mut actions,
                    );
                }
            }
        });

    if modal.should_close() {
        actions.push(UserAction::CancelAdminDialog);
    }
    actions
}

pub fn update_confirm_dialog(ctx: &egui::Context, confirming: bool) -> Vec<UserAction> {
    if !confirming {
        return Vec::new();
    }
    let mut actions = Vec::new();
    let frame = egui::Frame::window(&ctx.global_style())
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(18));

    let modal = egui::Modal::new(egui::Id::unique("update_confirm_dialog"))
        .backdrop_color(egui::Color32::from_black_alpha(180))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.set_max_width(400.0);
            ui.label(
                RichText::new("Atualizar Zenha")
                    .font(theme::display(18.0))
                    .color(theme::TEXT),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new("Atualizar agora? Você vai sair da sala.")
                    .color(theme::TEXT_SECONDARY),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.add(theme::accent_button("Atualizar")).clicked() {
                    actions.push(UserAction::ConfirmUpdate);
                }
                if ui.button("Cancelar").clicked() {
                    actions.push(UserAction::CancelUpdateConfirm);
                }
            });
        });

    if modal.should_close() {
        actions.push(UserAction::CancelUpdateConfirm);
    }
    actions
}

fn room_title_dialog_ui(
    ui: &mut egui::Ui,
    heading: &str,
    confirm_label: &str,
    title: &mut String,
    error: Option<&str>,
    actions: &mut Vec<UserAction>,
) {
    ui.label(
        RichText::new(heading)
            .font(theme::display(18.0))
            .color(theme::TEXT),
    );
    ui.add_space(10.0);
    let input = ui.add(
        egui::TextEdit::singleline(title)
            .char_limit(MAX_ROOM_TITLE_LEN)
            .desired_width(f32::INFINITY)
            .hint_text("Nome da sala"),
    );
    if ui.ctx().memory(|m| m.focused().is_none()) {
        input.request_focus();
    }
    if let Some(err) = error {
        ui.add_space(6.0);
        ui.colored_label(theme::ERROR, err);
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.add(theme::accent_button(confirm_label)).clicked() || enter {
            actions.push(UserAction::ConfirmAdminDialog);
        }
        if ui.button("Cancelar").clicked() {
            actions.push(UserAction::CancelAdminDialog);
        }
    });
}

fn confirm_danger_dialog_ui(
    ui: &mut egui::Ui,
    heading: &str,
    body: &str,
    confirm_label: &str,
    actions: &mut Vec<UserAction>,
) {
    ui.label(
        RichText::new(heading)
            .font(theme::display(18.0))
            .color(theme::TEXT),
    );
    ui.add_space(8.0);
    ui.label(RichText::new(body).color(theme::TEXT_SECONDARY));
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::Button::new(RichText::new(confirm_label).color(Color32::WHITE))
                    .fill(theme::ERROR),
            )
            .clicked()
        {
            actions.push(UserAction::ConfirmAdminDialog);
        }
        if ui.button("Cancelar").clicked() {
            actions.push(UserAction::CancelAdminDialog);
        }
    });
}

pub(super) fn chat_toggle_button(ui: &mut egui::Ui, open: bool, unread: usize) -> bool {
    let border = if open { theme::ACCENT } else { theme::BORDER };
    let mut text = egui::text::LayoutJob::default();
    RichText::new("Chat")
        .font(theme::semibold(13.0))
        .color(theme::TEXT)
        .append_to(
            &mut text,
            ui.style(),
            egui::FontSelection::Default,
            Align::Center,
        );
    if unread > 0 {
        RichText::new(format!("  {unread}"))
            .font(theme::semibold(13.0))
            .color(theme::ACCENT)
            .append_to(
                &mut text,
                ui.style(),
                egui::FontSelection::Default,
                Align::Center,
            );
    }
    ui.add(egui::Button::new(text).stroke(Stroke::new(1.0, border)))
        .clicked()
}

fn people_count_label(count: usize) -> String {
    if count == 1 {
        "1 pessoa na sala".to_owned()
    } else {
        format!("{count} pessoas na sala")
    }
}

fn local_display_name<'a>(participants: &'a [ParticipantView], local_identity: &'a str) -> &'a str {
    participants
        .iter()
        .find(|p| !local_identity.is_empty() && p.identity == local_identity)
        .map_or_else(
            || {
                if local_identity.is_empty() {
                    "Você"
                } else {
                    local_identity
                }
            },
            |p| p.name.as_str(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn participant(identity: &str, name: &str) -> ParticipantView {
        ParticipantView {
            identity: identity.to_owned(),
            name: name.to_owned(),
            muted: false,
            speaking: false,
        }
    }

    #[test]
    fn formats_people_count_in_portuguese() {
        assert_eq!(people_count_label(0), "0 pessoas na sala");
        assert_eq!(people_count_label(1), "1 pessoa na sala");
        assert_eq!(people_count_label(4), "4 pessoas na sala");
    }

    #[test]
    fn resolves_local_display_name_from_participants_or_fallback() {
        let list = vec![participant("id-1", "Lucas")];
        assert_eq!(local_display_name(&list, "id-1"), "Lucas");
        assert_eq!(local_display_name(&list, "id-2"), "id-2");
        assert_eq!(local_display_name(&list, ""), "Você");
    }
}
