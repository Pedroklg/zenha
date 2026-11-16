use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::chat_images::{ChatImages, ViewerImage, pending_paste_thumb_id};
use crate::cli::{self, StartupLink};
use crate::clipboard_image::{self, PasteImageError, SavedPastedImage};
use crate::core_session::CoreSession;
use crate::credentials;
use crate::demo::DemoSession;
use crate::dialogs;
use crate::format::server_host;
#[cfg(windows)]
use crate::global_keys::GlobalKeys;
#[cfg(windows)]
use crate::hotkeys::HotkeyAction;
#[cfg(windows)]
use crate::settings::VoiceMode;
use crate::settings::{self, Settings};
#[cfg(windows)]
use crate::single_instance::Listener;
use crate::state::{
    AppState, ChatEntry, CredentialOp, PendingPastedImage, Screen, UpdateEffect, UserAction,
    display_name_error, room_display_title,
};
use crate::theme;
#[cfg(windows)]
use crate::tray::{Tray, TrayAction};
use crate::ui_model::{SessionPort, UiCommand};
use crate::update::{self, AvailableUpdate, UpdateDownloadEvent};
use crate::views;

const REPAINT_INTERVAL: Duration = Duration::from_millis(100);
const VIDEO_REPAINT_INTERVAL: Duration = Duration::from_millis(33);
const DEMO_DOWNLOAD_DURATION: Duration = Duration::from_millis(1800);
const MAX_DROPPED_FILES: usize = 5;
const DEMO_LABEL: &str = "modo demonstração";
#[cfg(not(windows))]
const MEDIA_LABEL: Option<&str> =
    Some("sem mídia: servidor de tokens real, backend do LiveKit disponível só no Windows");
#[cfg(windows)]
const MEDIA_LABEL: Option<&str> = None;

struct DemoUpdateDownload {
    id: u64,
    version: String,
    total: u64,
    started_at: Instant,
    duration: Duration,
}

pub struct App {
    state: AppState,
    session: Box<dyn SessionPort>,
    settings_path: Option<PathBuf>,
    is_demo: bool,
    banner: Option<&'static str>,
    renderer: views::share::VideoRenderer,
    preview_renderer: views::share::VideoRenderer,
    picker_thumbnails: views::share::ThumbnailCache,
    images: ChatImages,
    viewer_image: ViewerImage,
    save_results: Receiver<Result<PathBuf, ()>>,
    save_sender: Sender<Result<PathBuf, ()>>,
    open_results: Receiver<Result<(), ()>>,
    open_sender: Sender<Result<(), ()>>,
    paste_results: Receiver<Result<SavedPastedImage, PasteImageError>>,
    paste_sender: Sender<Result<SavedPastedImage, PasteImageError>>,
    update_rx: Option<Receiver<AvailableUpdate>>,
    update_download_tx: Sender<UpdateDownloadEvent>,
    update_download_rx: Receiver<UpdateDownloadEvent>,
    active_download: Option<update::DownloadHandle>,
    demo_download: Option<DemoUpdateDownload>,
    #[cfg(windows)]
    forwarded_links: Option<Receiver<String>>,
    #[cfg(windows)]
    global_keys: Option<GlobalKeys>,
    #[cfg(windows)]
    tray: Option<Tray>,
    hidden_to_tray: bool,
    ui_visible: bool,
    quitting: bool,
    fullscreen: bool,
}

impl App {
    pub fn new(
        ctx: &egui::Context,
        settings: &Settings,
        saved_password: Option<String>,
        settings_path: Option<PathBuf>,
        demo_flag: bool,
        startup_link: StartupLink,
        #[cfg(windows)] instance: Option<Listener>,
    ) -> Self {
        theme::install(ctx);
        let (mut session, banner): (Box<dyn SessionPort>, _) = if demo_flag {
            #[cfg(test)]
            let demo_session = DemoSession::with_delays(crate::demo::DemoDelays::instant());
            #[cfg(not(test))]
            let demo_session = DemoSession::new();
            (Box::new(demo_session), Some(DEMO_LABEL))
        } else {
            let ctx = ctx.clone();
            let wake = Arc::new(move || ctx.request_repaint());
            (Box::new(CoreSession::new(wake)), MEDIA_LABEL)
        };
        let mut state = AppState::new(settings, saved_password);
        state.apply_startup_link(startup_link);
        if matches!(state.screen, Screen::Lobby) {
            session.configure(state.credentials());
        }
        let (save_sender, save_results) = channel();
        let (open_sender, open_results) = channel();
        let (paste_sender, paste_results) = channel();
        let (update_download_tx, update_download_rx) = channel();
        let update_rx = if demo_flag {
            state.set_can_auto_update(true);
            state.set_available_update(update::demo_available_update());
            None
        } else {
            state.set_can_auto_update(update::is_installed_exe());
            update::spawn_check(ctx.clone())
        };
        #[cfg(windows)]
        let forwarded_links = instance.map(|listener| {
            let _ = listener.ctx.set(ctx.clone());
            listener.links
        });
        #[cfg(windows)]
        let global_keys = (!cfg!(test)).then(|| GlobalKeys::new(ctx)).flatten();
        #[cfg(windows)]
        let tray = (!cfg!(test)).then(|| Tray::new(ctx)).flatten();
        let mut app = Self {
            state,
            session,
            settings_path,
            is_demo: demo_flag,
            banner,
            renderer: views::share::VideoRenderer::default(),
            preview_renderer: views::share::VideoRenderer::default(),
            picker_thumbnails: views::share::ThumbnailCache::default(),
            images: ChatImages::default(),
            viewer_image: ViewerImage::default(),
            save_results,
            save_sender,
            open_results,
            open_sender,
            paste_results,
            paste_sender,
            update_rx,
            update_download_tx,
            update_download_rx,
            active_download: None,
            demo_download: None,
            #[cfg(windows)]
            forwarded_links,
            #[cfg(windows)]
            global_keys,
            #[cfg(windows)]
            tray,
            hidden_to_tray: false,
            ui_visible: true,
            quitting: false,
            fullscreen: false,
        };
        app.flush_state_effects(ctx);
        app
    }

    fn is_hidden(&self, ctx: &egui::Context) -> bool {
        let (minimized, visible) = ctx.input(|i| (i.viewport().minimized, i.viewport().visible()));
        window_is_hidden(self.hidden_to_tray, minimized, visible)
    }

    fn sync_ui_visible(&mut self, ctx: &egui::Context) {
        let visible = !self.is_hidden(ctx);
        if visible != self.ui_visible {
            self.ui_visible = visible;
            self.session.send(UiCommand::SetUiVisible(visible));
        }
    }

    fn reset_renderers_outside_room(&mut self) {
        if !matches!(self.state.screen, Screen::InRoom(_)) {
            self.renderer.reset();
            self.preview_renderer.reset();
        }
    }

    #[cfg(windows)]
    fn bring_to_front(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn has_tray(&self) -> bool {
        #[cfg(windows)]
        {
            self.tray.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    fn pump_session(&mut self, ctx: &egui::Context) {
        while let Some(event) = self.session.try_recv() {
            self.state.apply_event(event);
        }
        if let Some(command) = self.state.poll_refresh(Instant::now()) {
            self.session.send(command);
        }
        self.flush_state_effects(ctx);
    }

    fn pump_update(&mut self) {
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

    fn pump_update_download(&mut self, ctx: &egui::Context) {
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

    fn pump_demo_update(&mut self, ctx: &egui::Context) {
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
            let (_, exe_path) = update::installer_paths(&update::update_temp_dir(), &version)
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
    fn finish_demo_download_now(&mut self) {
        if let Some(demo) = &mut self.demo_download {
            demo.duration = Duration::ZERO;
        }
    }

    #[cfg(windows)]
    fn pump_forwarded_links(&mut self, ctx: &egui::Context) {
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

    fn send_commands(&mut self, commands: Vec<UiCommand>) {
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

    fn flush_state_effects(&mut self, ctx: &egui::Context) {
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
                        self.active_download = update::spawn_download(
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
                        match update::launch_installer(&path) {
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

    fn resolve_effects(
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

    fn copy_in_background(&self, ctx: &egui::Context, source: PathBuf, dest: PathBuf) {
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

    fn pump_saves(&mut self) {
        while let Ok(result) = self.save_results.try_recv() {
            let text = match result {
                Ok(dest) => format!("Arquivo salvo em {}", dest.display()),
                Err(()) => "Não foi possível salvar o arquivo".to_owned(),
            };
            self.state.set_chat_notice(text);
        }
    }

    fn open_in_background(&self, ctx: &egui::Context, path: PathBuf) {
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

    fn pump_image_opens(&mut self) {
        while let Ok(result) = self.open_results.try_recv() {
            if result.is_err() {
                self.state
                    .set_image_viewer_error("Não foi possível abrir no Windows.");
            }
        }
    }

    fn prune_images(&mut self) {
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

    fn paste_image_in_background(&self, ctx: &egui::Context, image: Arc<egui::ColorImage>) {
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

    fn pump_pasted_images(&mut self, ctx: &egui::Context) {
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

    fn dispatch(&mut self, ctx: &egui::Context, actions: Vec<UserAction>) {
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

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_session(ctx);
        self.reset_renderers_outside_room();
        self.pump_update();
        self.pump_update_download(ctx);
        self.pump_demo_update(ctx);
        #[cfg(windows)]
        self.pump_forwarded_links(ctx);
        let now = Instant::now();
        #[cfg(windows)]
        if let Some(keys) = &mut self.global_keys {
            let ptt = self.state.audio.voice_mode == VoiceMode::PushToTalk;
            if let Some(failed) = keys.sync(&self.state.hotkeys.registrations(ptt)) {
                self.state.set_hotkey_failures(failed);
            }
            for (action, pressed) in keys.events() {
                match action {
                    HotkeyAction::ToggleMute | HotkeyAction::ToggleDeafen => {
                        if pressed {
                            self.dispatch(ctx, vec![UserAction::HotkeyPressed(action)]);
                        }
                    }
                    HotkeyAction::PushToTalk => {
                        for command in self.state.push_to_talk(pressed, now) {
                            self.session.send(command);
                        }
                    }
                }
            }
        }
        let (commands, wait) = self.state.poll_push_to_talk(now);
        for command in commands {
            self.session.send(command);
        }
        if let Some(wait) = wait {
            ctx.request_repaint_after(wait);
        }
        #[cfg(windows)]
        if let Some(actions) = self.tray.as_mut().map(Tray::actions) {
            for action in actions {
                match action {
                    TrayAction::Open => self.bring_to_front(ctx),
                    TrayAction::ToggleMute => {
                        self.dispatch(ctx, vec![UserAction::ToggleMute]);
                    }
                    TrayAction::Quit => {
                        self.quitting = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            if let Some(tray) = &mut self.tray {
                tray.sync(self.state.tray_muted());
            }
        }
        self.sync_ui_visible(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let hidden = self.is_hidden(ui.ctx());
        self.renderer.set_paused(hidden);
        self.preview_renderer
            .set_paused(hidden || self.state.is_share_preview_collapsed());
        self.reset_renderers_outside_room();
        self.pump_saves();
        self.pump_image_opens();
        self.pump_pasted_images(ui.ctx());
        if let Some(label) = self.banner {
            views::screens::demo_banner(ui, label);
        }

        let had_popup_before_views =
            ui.ctx().any_popup_open() || egui::Popup::is_any_open(ui.ctx());
        let prev_text_edit_focused = ui.ctx().data(|d| {
            d.get_temp::<bool>(prev_text_edit_focus_id())
                .unwrap_or(false)
        });
        let had_hotkey_capture = self.state.hotkeys.capturing.is_some();

        let show_password = self.state.show_password_field();
        let ptt_hint = self.state.push_to_talk_hint();
        let update_banner = self.state.visible_update_banner();
        let had_admin_dialog = self.state.admin.dialog.is_some();
        let had_update_dialog = self.state.is_update_confirming();
        let can_save_name = self.state.can_save_name();
        let name_error = display_name_error(&self.state.name_draft);
        let AppState {
            screen,
            form,
            name_draft,
            onboarding,
            lobby_voice,
            lobby_error,
            lobby_notice,
            rooms,
            audio,
            hotkeys,
            close_to_tray,
            room_sounds,
            muted_on_join,
            invite_copied,
            picker,
            admin,
            ..
        } = &mut self.state;
        let can_admin = !matches!(screen, Screen::Onboarding);
        let is_admin = admin.active;
        let renderer = &mut self.renderer;
        let preview_renderer = &mut self.preview_renderer;
        let images = &mut self.images;
        let mut actions = match screen {
            Screen::Onboarding => views::onboarding::show(ui, form, onboarding, show_password),
            Screen::Lobby => views::room::show_lobby(
                ui,
                &form.display_name,
                *lobby_voice,
                rooms,
                server_host(&form.server_url),
                lobby_error.as_deref(),
                lobby_notice.as_deref(),
                update_banner.as_ref(),
                is_admin,
            ),
            Screen::Connecting { room } => {
                views::screens::connecting(ui, room_display_title(rooms, room))
            }
            Screen::InRoom(room) => views::room::show(
                ui,
                room,
                rooms,
                server_host(&form.server_url),
                renderer,
                preview_renderer,
                images,
                audio.stream_volume,
                &audio.participant_volumes,
                ptt_hint.as_deref(),
                update_banner.as_ref(),
                is_admin,
            ),
        };
        actions.extend(views::settings::show(
            ui.ctx(),
            views::settings::SettingsView {
                server_url: &form.server_url,
                name_draft,
                name_error,
                can_save_name,
                audio,
                hotkeys,
                close_to_tray: *close_to_tray,
                room_sounds: *room_sounds,
                muted_on_join: *muted_on_join,
                invite_copied: *invite_copied,
                can_admin,
                admin,
            },
        ));
        actions.extend(views::room::admin_dialog(ui.ctx(), &mut admin.dialog));
        actions.extend(views::room::update_confirm_dialog(
            ui.ctx(),
            had_update_dialog,
        ));
        actions.extend(views::share::picker(
            ui.ctx(),
            picker,
            &mut self.picker_thumbnails,
        ));
        let open_viewer = match screen {
            Screen::InRoom(room) => room.chat.viewer.as_ref(),
            _ => None,
        };
        actions.extend(views::chat::viewer(
            ui.ctx(),
            open_viewer,
            &mut self.viewer_image,
        ));
        let already_toggled_fullscreen = actions
            .iter()
            .any(|action| matches!(action, UserAction::ToggleFullscreen));
        let blocking_overlay_open = had_popup_before_views
            || had_admin_dialog
            || had_update_dialog
            || self.state.admin.dialog.is_some()
            || self.state.is_update_confirming()
            || ui.ctx().any_popup_open()
            || egui::Popup::is_any_open(ui.ctx())
            || had_hotkey_capture;
        let text_input_focused = ui.ctx().text_edit_focused() || prev_text_edit_focused;
        let paste_action_triggered = actions.iter().any(|action| {
            matches!(
                action,
                UserAction::SendPastedImage | UserAction::CancelPastedImage | UserAction::SendChat
            )
        });
        if self.state.has_pending_paste()
            && self.state.can_paste_chat_image()
            && !blocking_overlay_open
            && !paste_action_triggered
        {
            if ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                actions.push(UserAction::CancelPastedImage);
            } else if self.state.chat_draft_is_empty()
                && ui.ctx().memory(|m| m.focused()).is_none()
                && ui
                    .ctx()
                    .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                actions.push(UserAction::SendPastedImage);
            }
        }
        let fullscreen = self.state.is_fullscreen();
        if fullscreen
            && !already_toggled_fullscreen
            && !blocking_overlay_open
            && !text_input_focused
            && ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            actions.push(UserAction::ToggleFullscreen);
        }
        let text_edit_focused = ui.ctx().text_edit_focused();
        ui.ctx()
            .data_mut(|d| d.insert_temp(prev_text_edit_focus_id(), text_edit_focused));
        let actions = self.resolve_effects(ui.ctx(), actions);
        self.dispatch(ui.ctx(), actions);
        self.prune_images();
        self.reset_renderers_outside_room();

        if ui.ctx().input(|i| i.viewport().close_requested())
            && self.state.close_to_tray
            && self.has_tray()
            && !self.quitting
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.hidden_to_tray = true;
            self.renderer.set_paused(true);
            self.preview_renderer.set_paused(true);
            self.sync_ui_visible(ui.ctx());
        }

        let fullscreen = self.state.is_fullscreen();
        if fullscreen != self.fullscreen {
            self.fullscreen = fullscreen;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
        }
        if !self.is_hidden(ui.ctx()) {
            let interval = if self.state.is_watching()
                || (self.state.is_sharing_local() && !self.state.is_share_preview_collapsed())
            {
                VIDEO_REPAINT_INTERVAL
            } else {
                REPAINT_INTERVAL
            };
            ui.ctx().request_repaint_after(interval);
        }
    }
}

fn prev_text_edit_focus_id() -> egui::Id {
    egui::Id::unique("prev_text_edit_focused")
}

fn window_is_hidden(hidden_to_tray: bool, minimized: Option<bool>, visible: Option<bool>) -> bool {
    hidden_to_tray || minimized == Some(true) || visible == Some(false)
}

fn open_path_in_os(path: &Path) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use eframe::egui;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::{App, open_path_in_os, window_is_hidden};
    use crate::cli::StartupLink;
    use crate::demo;
    use crate::settings::Settings;
    use crate::state::{Screen, UserAction};

    #[test]
    fn open_path_in_os_rejects_missing_and_directory_paths() {
        let missing = std::env::temp_dir().join(format!(
            "zenha-missing-viewer-image-{}.png",
            std::process::id()
        ));
        assert!(open_path_in_os(&missing).is_err());
        assert!(open_path_in_os(&std::env::temp_dir()).is_err());
    }

    #[test]
    fn window_is_hidden_covers_minimized_occluded_and_tray() {
        assert!(window_is_hidden(true, Some(false), None));
        assert!(window_is_hidden(false, Some(true), Some(false)));
        assert!(window_is_hidden(false, None, Some(false)));
        assert!(!window_is_hidden(false, Some(false), None));
        assert!(!window_is_hidden(false, Some(false), Some(true)));
    }

    #[test]
    fn demo_app_renders_lobby_room_settings_and_chat_frames() {
        let (tx, rx) = channel();
        let handle = std::thread::spawn(move || {
            let settings = Settings {
                client_id: demo::SELF_IDENTITY.to_owned(),
                display_name: demo::SELF_NAME.to_owned(),
                token_server_url: demo::DEMO_SERVER_URL.to_owned(),
                ..Settings::default()
            };
            let mut harness = Harness::builder()
                .with_size(egui::Vec2::new(960.0, 600.0))
                .with_accessibility_check(false)
                .build_eframe(move |cc| {
                    App::new(
                        &cc.egui_ctx,
                        &settings,
                        Some(demo::DEMO_PASSWORD.to_owned()),
                        None,
                        true,
                        StartupLink::None,
                        #[cfg(windows)]
                        None,
                    )
                });

            harness.run_steps(2);
            assert!(matches!(harness.state().state.screen, Screen::Lobby));
            assert_eq!(harness.state().state.rooms.len(), 3);

            harness.get_by_label("Geral").click();
            harness.run_steps(2);
            assert!(matches!(
                &harness.state().state.screen,
                Screen::InRoom(room) if room.room == "geral" && room.chat.open
            ));
            let sidebar_y = |name: &str| {
                harness
                    .query_all_by_label(name)
                    .map(|node| node.rect())
                    .min_by(|a, b| a.left().total_cmp(&b.left()))
                    .map(|rect| rect.center().y)
            };
            if let (Some(room), Some(ana)) = (sidebar_y("Geral"), sidebar_y("Ana")) {
                assert!(
                    ana - room < 80.0,
                    "sidebar room at {room}, occupant at {ana}"
                );
            }

            harness.get_by_label("Configurações").click();
            harness.run_steps(2);
            assert!(harness.state().state.audio.open);

            let ctx = harness.ctx.clone();
            harness
                .state_mut()
                .dispatch(&ctx, vec![UserAction::ToggleChat]);
            harness.run_steps(2);
            assert!(matches!(
                &harness.state().state.screen,
                Screen::InRoom(room) if !room.chat.open
            ));

            harness.state_mut().dispatch(
                &ctx,
                vec![UserAction::ToggleChat, UserAction::CloseSettings],
            );
            harness.run_steps(2);
            assert!(!harness.state().state.audio.open);
            assert!(matches!(
                &harness.state().state.screen,
                Screen::InRoom(room) if room.chat.open
            ));

            harness.get_by_label("Sair da sala").click();
            harness.run_steps(2);
            assert!(matches!(harness.state().state.screen, Screen::Lobby));

            let _ = tx.send(());
        });

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => {
                if let Err(payload) = handle.join() {
                    std::panic::resume_unwind(payload);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if let Err(payload) = handle.join() {
                    std::panic::resume_unwind(payload);
                }
                panic!("demo UI smoke test thread disconnected unexpectedly");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("demo UI smoke test timed out after 5s (possible egui Context deadlock)");
            }
        }
    }

    #[test]
    fn settings_window_opens_without_overlapping_widgets_at_1280x720() {
        let settings = Settings {
            client_id: demo::SELF_IDENTITY.to_owned(),
            display_name: demo::SELF_NAME.to_owned(),
            token_server_url: demo::DEMO_SERVER_URL.to_owned(),
            noise_suppression: crate::settings::NoiseMode::Enhanced,
            noise_gate_db: Some(-45),
            ..Settings::default()
        };
        let mut harness = Harness::builder()
            .with_size(egui::Vec2::new(1280.0, 720.0))
            .with_accessibility_check(false)
            .build_eframe(move |cc| {
                App::new(
                    &cc.egui_ctx,
                    &settings,
                    Some(demo::DEMO_PASSWORD.to_owned()),
                    None,
                    true,
                    StartupLink::None,
                    #[cfg(windows)]
                    None,
                )
            });

        harness.run_steps(2);
        harness.get_by_label("Geral").click();
        harness.run_steps(2);
        harness
            .state_mut()
            .state
            .apply_event(crate::ui_model::UiEvent::MicLevel {
                level_db: -35,
                gate_open: true,
            });
        harness.get_by_label("Configurações").click();
        harness.run_steps(3);
        assert!(harness.state().state.audio.open);

        let audio_rect = harness.get_by_label("Áudio").rect();
        let settings_node_rect = |label: &str| {
            harness
                .query_all_by_label(label)
                .map(|node| node.rect())
                .filter(|rect| rect.left() >= audio_rect.left() - 2.0)
                .max_by(|a, b| a.top().total_cmp(&b.top()))
                .unwrap_or_else(|| panic!("missing settings node {label:?}"))
        };

        let labels = [
            "Áudio",
            "Microfone",
            "Saída de áudio",
            "Atualizar lista",
            "Modo de voz",
            "Supressão de ruído",
            "Porta de ruído (corta o som abaixo do limiar)",
            "Sensibilidade",
            "Nível do microfone",
            "Atalhos globais",
            "Ligar/desligar microfone",
            "Ligar/desligar áudio",
            "Geral",
            "Servidor",
            "Administração",
        ];
        let mut rects = Vec::with_capacity(labels.len());
        for label in labels {
            let rect = settings_node_rect(label);
            assert!(
                rect.height() < 36.0,
                "label {label:?} wrapped unexpectedly: {rect:?}"
            );
            assert!(
                rect.right() <= 1280.0 && rect.left() >= 0.0,
                "label {label:?} out of bounds: {rect:?}"
            );
            rects.push((label, rect));
        }
        for section in [
            "Áudio",
            "Atalhos globais",
            "Geral",
            "Servidor",
            "Administração",
        ] {
            let rect = settings_node_rect(section);
            assert!(
                (rect.left() - audio_rect.left()).abs() < 2.0,
                "section {section:?} misaligned horizontally: {rect:?} vs {audio_rect:?}"
            );
        }
        let threshold_db = settings_node_rect("-45 dB");
        let level_db = settings_node_rect("-35 dB");
        assert!(
            (threshold_db.right() - level_db.right()).abs() < 2.0,
            "dB values misaligned: {threshold_db:?} vs {level_db:?}"
        );
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let (la, ra) = rects[i];
                let (lb, rb) = rects[j];
                assert!(
                    !ra.intersects(rb),
                    "settings widgets overlap: {la:?} ({ra:?}) vs {lb:?} ({rb:?})"
                );
            }
        }
        for window in rects.windows(2) {
            let (la, ra) = window[0];
            let (lb, rb) = window[1];
            assert!(
                ra.bottom() <= rb.top(),
                "expected {la:?} ({ra:?}) above {lb:?} ({rb:?})"
            );
        }
    }

    #[test]
    fn pasted_image_card_allows_sending_with_enter_and_canceling_with_esc() {
        let settings = Settings {
            client_id: demo::SELF_IDENTITY.to_owned(),
            display_name: demo::SELF_NAME.to_owned(),
            token_server_url: demo::DEMO_SERVER_URL.to_owned(),
            ..Settings::default()
        };
        let mut harness = Harness::builder()
            .with_size(egui::Vec2::new(1280.0, 720.0))
            .with_accessibility_check(false)
            .build_eframe(move |cc| {
                App::new(
                    &cc.egui_ctx,
                    &settings,
                    Some(demo::DEMO_PASSWORD.to_owned()),
                    None,
                    true,
                    StartupLink::None,
                    #[cfg(windows)]
                    None,
                )
            });

        harness.run_steps(2);
        harness.get_by_label("Geral").click();
        harness.run_steps(2);

        let dir = std::env::temp_dir().join(format!("zenha-kittest-paste-{}", std::process::id()));
        let image = egui::ColorImage::new([2, 2], vec![egui::Color32::RED; 4]);
        let saved = crate::clipboard_image::save_pasted_image(&image, &dir).unwrap();
        let saved_path = saved.path.clone();
        let saved_name = saved.name.clone();
        let ctx = harness.ctx.clone();
        harness.state_mut().dispatch(
            &ctx,
            vec![UserAction::StagePastedImage(
                crate::state::PendingPastedImage {
                    path: saved.path,
                    name: saved.name,
                    size: saved.size,
                },
            )],
        );
        harness.run_steps(2);

        assert!(harness.query_by_label(saved_name.as_str()).is_some());
        assert!(harness.query_by_label("Enviar").is_some());
        assert!(harness.query_by_label("Cancelar").is_some());

        harness.key_press(egui::Key::Escape);
        harness.run_steps(2);
        assert!(!harness.state().state.has_pending_paste());
        assert!(harness.query_by_label("Enviar").is_none());

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while saved_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !saved_path.exists(),
            "canceling pasted image should delete temporary PNG"
        );

        let saved_enter = crate::clipboard_image::save_pasted_image(&image, &dir).unwrap();
        let saved_enter_path = saved_enter.path.clone();
        harness.state_mut().dispatch(
            &ctx,
            vec![UserAction::StagePastedImage(
                crate::state::PendingPastedImage {
                    path: saved_enter.path,
                    name: saved_enter.name,
                    size: saved_enter.size,
                },
            )],
        );
        harness.run_steps(2);
        assert!(harness.state().state.has_pending_paste());

        if let Screen::InRoom(room) = &mut harness.state_mut().state.screen {
            room.chat.draft = "legenda".to_owned();
        }
        harness.run_steps(1);
        harness.key_press(egui::Key::Enter);
        harness.run_steps(2);
        assert!(harness.state().state.has_pending_paste());

        if let Screen::InRoom(room) = &mut harness.state_mut().state.screen {
            room.chat.draft.clear();
        }
        harness.run_steps(1);
        harness.key_press(egui::Key::Enter);
        harness.run_steps(2);
        assert!(!harness.state().state.has_pending_paste());
        assert!(saved_enter_path.exists());

        let _ = std::fs::remove_file(saved_enter_path);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn local_share_preview_collapses_and_expands_in_room_ui() {
        let settings = Settings {
            client_id: demo::SELF_IDENTITY.to_owned(),
            display_name: demo::SELF_NAME.to_owned(),
            token_server_url: demo::DEMO_SERVER_URL.to_owned(),
            ..Settings::default()
        };
        let mut harness = Harness::builder()
            .with_size(egui::Vec2::new(1280.0, 720.0))
            .with_accessibility_check(false)
            .build_eframe(move |cc| {
                App::new(
                    &cc.egui_ctx,
                    &settings,
                    Some(demo::DEMO_PASSWORD.to_owned()),
                    None,
                    true,
                    StartupLink::None,
                    #[cfg(windows)]
                    None,
                )
            });

        harness.run_steps(2);
        harness.get_by_label("Geral").click();
        harness.run_steps(2);

        let ctx = harness.ctx.clone();
        harness
            .state_mut()
            .dispatch(&ctx, vec![UserAction::OpenSharePicker]);
        harness.run_steps(2);
        harness.get_by_label("Iniciar").click();
        harness.run_steps(2);

        assert!(harness.state().state.is_sharing_local());
        assert!(!harness.state().state.is_share_preview_collapsed());
        assert!(harness.query_by_label("Você está transmitindo").is_some());
        assert!(harness.query_by_label("Recolher").is_some());

        harness.get_by_label("Recolher").click();
        harness.run_steps(2);
        assert!(harness.state().state.is_share_preview_collapsed());
        assert!(harness.query_by_label("Recolher").is_none());
        assert!(harness.query_by_label("Você está transmitindo").is_some());
        assert!(harness.query_by_label("Expandir").is_some());

        harness.get_by_label("Expandir").click();
        harness.run_steps(2);
        assert!(!harness.state().state.is_share_preview_collapsed());
        assert!(harness.query_by_label("Recolher").is_some());
    }

    #[test]
    fn demo_update_footer_prompts_in_room_shows_progress_and_completes() {
        let settings = Settings {
            client_id: demo::SELF_IDENTITY.to_owned(),
            display_name: demo::SELF_NAME.to_owned(),
            token_server_url: demo::DEMO_SERVER_URL.to_owned(),
            ..Settings::default()
        };
        let mut harness = Harness::builder()
            .with_size(egui::Vec2::new(960.0, 600.0))
            .with_accessibility_check(false)
            .build_eframe(move |cc| {
                App::new(
                    &cc.egui_ctx,
                    &settings,
                    Some(demo::DEMO_PASSWORD.to_owned()),
                    None,
                    true,
                    StartupLink::None,
                    #[cfg(windows)]
                    None,
                )
            });

        harness.run_steps(2);
        assert!(harness.query_by_label("Atualizar").is_some());

        harness.get_by_label("Geral").click();
        harness.run_steps(2);
        assert!(matches!(harness.state().state.screen, Screen::InRoom(_)));

        harness.get_by_label("Atualizar").click();
        harness.run_steps(3);
        assert!(harness.state().state.is_update_confirming());
        assert!(
            harness
                .query_by_label("Atualizar agora? Você vai sair da sala.")
                .is_some()
        );

        harness.get_by_label("Cancelar").click();
        harness.run_steps(2);
        assert!(!harness.state().state.is_update_confirming());
        assert!(matches!(harness.state().state.screen, Screen::InRoom(_)));

        harness.get_by_label("Atualizar").click();
        harness.run_steps(3);
        harness.get_by_label("Atualizar").click();
        harness.run_steps(3);
        assert!(!harness.state().state.is_update_confirming());
        assert!(matches!(
            harness
                .state()
                .state
                .visible_update_banner()
                .map(|b| b.phase),
            Some(crate::state::UpdatePhase::Downloading { .. })
        ));
        assert!(harness.query_by_label("Cancelar").is_some());

        harness.get_by_label("Cancelar").click();
        harness.run_steps(3);
        assert!(harness.query_by_label("Atualizar").is_some());

        harness.get_by_label("Atualizar").click();
        harness.run_steps(3);
        harness.get_by_label("Atualizar").click();
        harness.run_steps(1);
        harness.state_mut().finish_demo_download_now();
        harness.run_steps(2);
        assert!(matches!(harness.state().state.screen, Screen::Lobby));
        assert!(harness.query_by_label("Pronto para instalar").is_some());
    }
}
