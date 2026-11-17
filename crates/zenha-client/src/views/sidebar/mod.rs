//! Barra lateral com lista de salas, ocupantes e rodapé do usuário.

mod footer;
mod rooms;

use std::collections::BTreeMap;

use eframe::egui::{self, Margin, RichText, Stroke};
use zenha_protocol::RoomSummary;

use super::room::HEADER_HEIGHT;
use crate::state::{ConnectionStatus, ShareState, UpdateBanner, UserAction};
use crate::theme;
use crate::ui_model::ParticipantView;
use footer::{update_footer, user_footer};
use rooms::{RoomsListView, rooms_list};

const SIDEBAR_WIDTH: f32 = 256.0;

pub(super) struct SidebarUser<'a> {
    pub(super) identity: &'a str,
    pub(super) avatar_version: Option<&'a str>,
    pub(super) name: &'a str,
    pub(super) connection: Option<ConnectionStatus>,
    pub(super) ptt_hint: Option<&'a str>,
    pub(super) muted: bool,
    pub(super) deafened: bool,
}

pub(super) struct SidebarArgs<'a> {
    pub(super) user: SidebarUser<'a>,
    pub(super) current_room: Option<&'a str>,
    pub(super) local_identity: &'a str,
    pub(super) participants: &'a [ParticipantView],
    pub(super) share: Option<&'a ShareState>,
    pub(super) participant_volumes: &'a BTreeMap<String, u16>,
    pub(super) rooms: &'a [RoomSummary],
    pub(super) server_host: &'a str,
    pub(super) update_banner: Option<&'a UpdateBanner>,
    pub(super) is_admin: bool,
}
pub(super) fn sidebar(ui: &mut egui::Ui, args: SidebarArgs<'_>, actions: &mut Vec<UserAction>) {
    egui::Panel::left("rooms_panel")
        .exact_size(SIDEBAR_WIDTH)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(Margin::ZERO),
        )
        .show(ui, |ui| {
            egui::Panel::top("sidebar_brand")
                .exact_size(HEADER_HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::symmetric(16, 10)),
                )
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(
                            RichText::new("Zenha")
                                .font(theme::display(20.0))
                                .color(theme::TEXT),
                        );
                        if !args.server_host.is_empty() {
                            ui.label(
                                RichText::new(args.server_host)
                                    .size(12.0)
                                    .color(theme::TEXT_WEAK),
                            );
                        }
                    });
                    let full = ui.clip_rect();
                    let bottom = ui.max_rect().bottom() + 10.0;
                    ui.painter()
                        .hline(full.x_range(), bottom, Stroke::new(1.0, theme::BORDER));
                });

            egui::Panel::bottom("sidebar_user_footer")
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL_DARK)
                        .inner_margin(Margin::same(12)),
                )
                .show(ui, |ui| {
                    let full = ui.clip_rect();
                    let top = ui.max_rect().top() - 12.0;
                    ui.painter()
                        .hline(full.x_range(), top, Stroke::new(1.0, theme::BORDER));
                    user_footer(ui, args.user, actions);
                    if let Some(banner) = args.update_banner {
                        ui.add_space(8.0);
                        update_footer(ui, banner, actions);
                    }
                });

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::same(12)),
                )
                .show(ui, |ui| {
                    rooms_list(
                        ui,
                        RoomsListView {
                            current_room: args.current_room,
                            local_identity: args.local_identity,
                            participants: args.participants,
                            share: args.share,
                            participant_volumes: args.participant_volumes,
                            rooms: args.rooms,
                            is_admin: args.is_admin,
                        },
                        actions,
                    );
                });
        });
}
