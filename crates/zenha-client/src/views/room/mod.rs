//! Telas de lobby e de sala ativa; cartões, avisos e diálogos ficam nos módulos filhos.

mod banners;
mod center;
mod dialogs;
mod participants;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use eframe::egui::{self, CornerRadius, Layout, Margin, RichText, Stroke};
use zenha_protocol::RoomSummary;

use super::chat;
use super::room_watch::{ShareRenderers, watching_center};
use super::share::VideoRenderer;
use super::sidebar::{SidebarArgs, SidebarUser, sidebar};
use crate::chat_images::ChatImages;
use crate::state::{LobbyVoiceState, RoomState, UpdateBanner, UserAction};
use crate::theme;
use crate::ui_model::ParticipantView;
use banners::dismissible_banner;
use center::main_center;

pub use dialogs::{admin_dialog, room_password_dialog, update_confirm_dialog};

pub(crate) use banners::{collapsed_chat_notice_banner, share_error_banner};
pub(crate) use center::chat_toggle_button;

pub(super) const HEADER_HEIGHT: f32 = 64.0;

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
    let local_avatar = state
        .participants
        .iter()
        .find(|p| p.identity == state.local_identity)
        .and_then(|p| p.avatar_version.as_deref());
    let user = SidebarUser {
        identity: &state.local_identity,
        avatar_version: local_avatar,
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
    avatar: (&str, Option<&str>),
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
        identity: avatar.0,
        avatar_version: avatar.1,
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
