//! Window visibility: hide to tray, minimize and bring to front.

use eframe::egui;

use crate::state::{LocalShare, Screen};
use crate::ui_model::UiCommand;

use super::App;

impl App {
    pub(super) fn is_hidden(&self, ctx: &egui::Context) -> bool {
        let (minimized, visible) = ctx.input(|i| (i.viewport().minimized, i.viewport().visible()));
        window_is_hidden(self.hidden_to_tray, minimized, visible)
    }

    pub(super) fn sync_ui_visible(&mut self, ctx: &egui::Context) {
        let visible = !self.is_hidden(ctx);
        if visible != self.ui_visible {
            self.ui_visible = visible;
            self.session.send(UiCommand::SetUiVisible(visible));
        }
    }

    pub(super) fn sync_renderers(&mut self) {
        match &self.state.screen {
            Screen::InRoom(room) => {
                if room.share.watching.is_none() {
                    self.renderer.reset();
                }
                if room.share.local != LocalShare::Active || room.share.preview_collapsed {
                    self.preview_renderer.reset();
                }
            }
            _ => {
                self.renderer.reset();
                self.preview_renderer.reset();
            }
        }
    }

    pub(super) fn prune_avatars(&mut self, ctx: &egui::Context) {
        if matches!(self.state.screen, Screen::Onboarding) {
            let local_id = self.state.client_id();
            self.avatars.clear_unused(ctx, |id| id == local_id);
        }
    }

    #[cfg(windows)]
    pub(super) fn bring_to_front(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    pub(super) fn has_tray(&self) -> bool {
        #[cfg(windows)]
        {
            self.tray.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

pub(super) fn window_is_hidden(
    hidden_to_tray: bool,
    minimized: Option<bool>,
    visible: Option<bool>,
) -> bool {
    hidden_to_tray || minimized == Some(true) || visible == Some(false)
}
