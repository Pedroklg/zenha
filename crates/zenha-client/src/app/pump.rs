//! Draining the async channels: session, update, forwarded links, saves and pasted images.

use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use eframe::egui;

#[cfg(windows)]
use crate::cli::{self, StartupLink};
use crate::state::{PendingPastedImage, Screen, UserAction};
use crate::ui_model::UiEvent;
use crate::updater::{self, UpdateDownloadEvent};

use super::App;

impl App {
    pub(super) fn pump_session(&mut self, ctx: &egui::Context) {
        self.avatars.drain(ctx);
        while let Some(event) = self.session.try_recv() {
            if let UiEvent::AvatarCached {
                identity,
                version,
                path,
            } = &event
            {
                self.avatars.register(ctx, identity, version, path.clone());
            }
            self.state.apply_event(event);
        }
        if let Some(command) = self.state.poll_refresh(Instant::now()) {
            self.session.send(command);
        }
        self.flush_state_effects(ctx);
    }

    pub(super) fn pump_update(&mut self) {
        let Some(rx) = &self.update_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(update) => {
                self.state.set_available_update(update);
                self.update_rx = None;
            }
            Err(TryRecvError::Disconnected) => {
                self.update_rx = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    pub(super) fn pump_personal_hook(&mut self) {
        while let Ok((id, result)) = self.personal_hook_rx.try_recv() {
            self.state.apply_personal_hook_result(id, result);
        }
    }

    pub(super) fn pump_update_download(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        while let Ok(event) = self.update_download_rx.try_recv() {
            events.push(event);
        }
        if events.is_empty() {
            return;
        }
        for event in events {
            if let UpdateDownloadEvent::Finished { id, .. } = &event
                && self
                    .active_download
                    .as_ref()
                    .is_some_and(|handle| handle.id() == *id)
            {
                self.active_download = None;
            }
            self.state.apply_update_event(event);
        }
        self.flush_state_effects(ctx);
    }

    pub(super) fn pump_demo_update(&mut self, ctx: &egui::Context) {
        let Some(demo) = &self.demo_download else {
            return;
        };
        let elapsed = demo.started_at.elapsed();
        if elapsed >= demo.duration {
            let id = demo.id;
            let total = demo.total;
            let version = demo.version.clone();
            self.demo_download = None;
            self.state
                .apply_update_event(UpdateDownloadEvent::Progress {
                    id,
                    downloaded: total,
                    total,
                });
            let (_, exe_path) = updater::installer_paths(&updater::update_temp_dir(), &version)
                .unwrap_or_else(|| (PathBuf::new(), PathBuf::from("Zenha-demo-setup.exe")));
            self.state
                .apply_update_event(UpdateDownloadEvent::Finished {
                    id,
                    result: Ok(exe_path),
                });
            self.flush_state_effects(ctx);
        } else {
            let duration_ms = demo.duration.as_millis().max(1);
            let downloaded = u64::try_from(
                u128::from(demo.total).saturating_mul(elapsed.as_millis()) / duration_ms,
            )
            .unwrap_or(demo.total)
            .min(demo.total);
            self.state
                .apply_update_event(UpdateDownloadEvent::Progress {
                    id: demo.id,
                    downloaded,
                    total: demo.total,
                });
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    #[cfg(test)]
    pub(super) fn finish_demo_download_now(&mut self) {
        if let Some(demo) = &mut self.demo_download {
            demo.duration = Duration::ZERO;
        }
    }

    #[cfg(windows)]
    pub(super) fn pump_forwarded_links(&mut self, ctx: &egui::Context) {
        let links: Vec<String> = self
            .forwarded_links
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for raw in links {
            self.bring_to_front(ctx);
            if matches!(
                self.state.screen,
                Screen::Connecting { .. } | Screen::InRoom(_)
            ) {
                continue;
            }
            let link = cli::parse_args([raw]).startup_link;
            let is_invite = matches!(link, StartupLink::Invite(_));
            self.state.apply_startup_link(link);
            if is_invite && matches!(self.state.screen, Screen::Lobby) {
                self.session.configure(self.state.credentials());
            }
            self.flush_state_effects(ctx);
        }
    }

    pub(super) fn pump_saves(&mut self) {
        while let Ok(result) = self.save_results.try_recv() {
            let text = match result {
                Ok(dest) => format!("Arquivo salvo em {}", dest.display()),
                Err(()) => "Não foi possível salvar o arquivo".to_owned(),
            };
            self.state.set_chat_notice(text);
        }
    }

    pub(super) fn pump_image_opens(&mut self) {
        while let Ok(result) = self.open_results.try_recv() {
            if result.is_err() {
                self.state
                    .set_image_viewer_error("Não foi possível abrir no Windows.");
            }
        }
    }

    pub(super) fn pump_pasted_images(&mut self, ctx: &egui::Context) {
        let mut staged = Vec::new();
        while let Ok(result) = self.paste_results.try_recv() {
            match result {
                Ok(saved) => staged.push(UserAction::StagePastedImage(PendingPastedImage {
                    path: saved.path,
                    name: saved.name,
                    size: saved.size,
                })),
                Err(err) => self.state.set_chat_notice(err.user_message().to_owned()),
            }
        }
        if !staged.is_empty() {
            self.dispatch(ctx, staged);
            self.prune_images();
        }
    }
}
