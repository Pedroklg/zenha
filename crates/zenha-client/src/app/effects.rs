//! Effects requested by state and user actions: commands, writing to disk, opening files and pasting.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::chat_images::pending_paste_thumb_id;
use crate::dialogs;
use crate::platform::clipboard_image::{self, PasteImageError};
use crate::platform::{credentials, settings};
use crate::state::{ChatEntry, CredentialOp, Screen, UpdateEffect, UserAction};
use crate::ui_model::UiCommand;
use crate::updater;

use super::{App, DemoUpdateDownload};

const DEMO_DOWNLOAD_DURATION: Duration = Duration::from_millis(1800);
const MAX_DROPPED_FILES: usize = 5;

impl App {
    pub(super) fn send_commands(&mut self, commands: Vec<UiCommand>) {
        if commands.is_empty() {
            return;
        }
        let now = Instant::now();
        for command in commands {
            if matches!(command, UiCommand::RefreshRooms) {
                self.state.mark_refreshed(now);
            }
            self.session.send(command);
        }
    }

    pub(super) fn flush_state_effects(&mut self, ctx: &egui::Context) {
        let update_effects = self.state.take_update_effects();
        for effect in update_effects {
            match effect {
                UpdateEffect::StartDownload { id, version, asset } => {
                    if self.is_demo {
                        self.demo_download = Some(DemoUpdateDownload {
                            id,
                            version,
                            total: asset.size,
                            started_at: Instant::now(),
                            duration: DEMO_DOWNLOAD_DURATION,
                        });
                        ctx.request_repaint();
                    } else {
                        self.active_download = updater::spawn_download(
                            ctx.clone(),
                            id,
                            version,
                            asset,
                            self.update_download_tx.clone(),
                        );
                    }
                }
                UpdateEffect::CancelDownload { id } => {
                    if self.is_demo {
                        if self.demo_download.as_ref().is_some_and(|d| d.id == id) {
                            self.demo_download = None;
                        }
                    } else if self
                        .active_download
                        .as_ref()
                        .is_some_and(|handle| handle.id() == id)
                    {
                        self.active_download = None;
                    }
                }
                UpdateEffect::RunInstaller { path } => {
                    if self.is_demo {
                        let cmds = self.state.leave_for_update();
                        self.send_commands(cmds);
                    } else {
                        match updater::launch_installer(&path) {
                            Ok(()) => {
                                let cmds = self.state.leave_for_update();
                                self.send_commands(cmds);
                                self.quitting = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            Err(err) => {
                                tracing::warn!(%err, "could not start update installer");
                                self.state.on_installer_launch_failed(
                                    "Não foi possível iniciar o instalador.".to_owned(),
                                );
                            }
                        }
                    }
                }
            }
        }
        let pending = self.state.take_pending_commands();
        self.send_commands(pending);
        for path in self.state.take_pending_paste_deletes() {
            clipboard_image::delete_in_background(path);
        }
        let credential_ops = self.state.take_credential_ops();
        let pending_save = self.state.take_pending_save();
        if self.is_demo {
            return;
        }
        for op in credential_ops {
            match op {
                CredentialOp::Store { server, password } => {
                    credentials::store_in_background(server, password);
                }
                CredentialOp::Forget { server } => {
                    credentials::forget_in_background(server);
                }
            }
        }
        if let (Some(settings), Some(path)) = (pending_save, self.settings_path.clone()) {
            settings::save_in_background(path, settings);
        }
    }

    pub(super) fn resolve_effects(
        &mut self,
        ctx: &egui::Context,
        actions: Vec<UserAction>,
    ) -> Vec<UserAction> {
        let mut resolved = Vec::with_capacity(actions.len());
        for action in actions {
            match action {
                UserAction::PickFile => {
                    resolved.extend(dialogs::pick_file().map(UserAction::AttachFile));
                }
                UserAction::PickAvatar => {
                    resolved.extend(dialogs::pick_avatar().map(UserAction::SetAvatar));
                }
                UserAction::SaveReceivedFile { source, name } => {
                    if let Some(dest) = dialogs::pick_save_path(&name) {
                        self.copy_in_background(ctx, source, dest);
                    }
                }
                UserAction::OpenImageExternally(path) => {
                    self.state.clear_image_viewer_error();
                    self.open_in_background(ctx, path);
                }
                other => resolved.push(other),
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .take(MAX_DROPPED_FILES)
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        resolved.extend(dropped.into_iter().map(UserAction::AttachFile));
        if self.state.can_paste_chat_image() && !ctx.will_discard() {
            let pasted: Vec<Arc<egui::ColorImage>> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::PasteImage(image) => Some(Arc::clone(image)),
                        _ => None,
                    })
                    .collect()
            });
            for image in pasted {
                self.paste_image_in_background(ctx, image);
            }
        }
        resolved
    }

    pub(super) fn copy_in_background(&self, ctx: &egui::Context, source: PathBuf, dest: PathBuf) {
        let sender = self.save_sender.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("save-received-file".to_owned())
            .spawn(move || {
                let result = match std::fs::copy(&source, &dest) {
                    Ok(_) => Ok(dest),
                    Err(err) => {
                        tracing::warn!(%err, "could not save received file");
                        Err(())
                    }
                };
                if sender.send(result).is_ok() {
                    ctx.request_repaint();
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "could not start save thread");
            let _ = self.save_sender.send(Err(()));
        }
    }

    pub(super) fn open_in_background(&self, ctx: &egui::Context, path: PathBuf) {
        let sender = self.open_sender.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("open-chat-image".to_owned())
            .spawn(move || {
                let result = open_path_in_os(&path).map_err(|err| {
                    tracing::warn!(
                        %err,
                        path = %path.display(),
                        "could not open chat image in OS viewer"
                    );
                });
                if sender.send(result).is_ok() {
                    ctx.request_repaint();
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "could not start OS viewer thread");
            let _ = self.open_sender.send(Err(()));
        }
    }

    pub(super) fn prune_images(&mut self) {
        self.viewer_image.retain_id(self.state.open_image_id());
        if self.images.is_empty() {
            return;
        }
        match &self.state.screen {
            Screen::InRoom(room) => {
                let ids: HashSet<&str> = room
                    .chat
                    .messages
                    .iter()
                    .filter_map(|entry| match entry {
                        ChatEntry::File(file) => Some(file.id.as_str()),
                        ChatEntry::Text(_) => None,
                    })
                    .collect();
                let pending_id = room
                    .chat
                    .pending_paste
                    .as_ref()
                    .map(|pending| pending_paste_thumb_id(&pending.path));
                self.images
                    .retain(|id| ids.contains(id) || pending_id.as_deref() == Some(id));
            }
            _ => self.images.retain(|_| false),
        }
    }

    pub(super) fn paste_image_in_background(
        &self,
        ctx: &egui::Context,
        image: Arc<egui::ColorImage>,
    ) {
        let sender = self.paste_sender.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("paste-clipboard-image".to_owned())
            .spawn(move || {
                let dir = clipboard_image::paste_temp_dir();
                let result = clipboard_image::save_pasted_image(&image, &dir);
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "could not start clipboard image thread");
            let _ = self.paste_sender.send(Err(PasteImageError::PrepareFailed));
        }
    }

    pub(super) fn dispatch(&mut self, ctx: &egui::Context, actions: Vec<UserAction>) {
        if actions.contains(&UserAction::SubmitOnboarding) && self.state.can_submit_onboarding() {
            self.session.configure(self.state.credentials());
        }
        if actions.contains(&UserAction::CopyInviteLink) {
            ctx.copy_text(self.state.invite_link());
        }
        let commands: Vec<UiCommand> = actions
            .into_iter()
            .flat_map(|action| self.state.handle_action(action))
            .collect();
        self.send_commands(commands);
        self.flush_state_effects(ctx);
    }
}

pub(super) fn open_path_in_os(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "image path is not a file",
        ));
    }
    let target = std::path::absolute(path)?;
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(&target)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(&target).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&target)
            .spawn()?;
    }
    Ok(())
}
