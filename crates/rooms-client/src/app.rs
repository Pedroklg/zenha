use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::chat_images::ChatImages;
use crate::core_session::CoreSession;
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
use crate::state::{AppState, ChatEntry, Screen, UserAction};
use crate::theme;
#[cfg(windows)]
use crate::tray::{Tray, TrayAction};
use crate::ui_model::{SessionPort, UiCommand};
use crate::views;

const REPAINT_INTERVAL: Duration = Duration::from_millis(100);
const VIDEO_REPAINT_INTERVAL: Duration = Duration::from_millis(33);
const MAX_DROPPED_FILES: usize = 5;
const DEMO_LABEL: &str = "modo demonstração";
#[cfg(not(windows))]
const MEDIA_LABEL: Option<&str> =
    Some("sem mídia: servidor de tokens real, backend do LiveKit disponível só no Windows");
#[cfg(windows)]
const MEDIA_LABEL: Option<&str> = None;

pub struct App {
    state: AppState,
    session: Box<dyn SessionPort>,
    settings_path: Option<PathBuf>,
    banner: Option<&'static str>,
    renderer: views::share::VideoRenderer,
    images: ChatImages,
    save_results: Receiver<Result<PathBuf, ()>>,
    save_sender: Sender<Result<PathBuf, ()>>,
    #[cfg(windows)]
    global_keys: Option<GlobalKeys>,
    #[cfg(windows)]
    tray: Option<Tray>,
    quitting: bool,
    fullscreen: bool,
}

impl App {
    pub fn new(
        ctx: &egui::Context,
        settings: &Settings,
        settings_path: Option<PathBuf>,
        demo_flag: bool,
    ) -> Self {
        theme::install(ctx);
        let (session, banner): (Box<dyn SessionPort>, _) = if demo_flag {
            (Box::new(DemoSession::new()), Some(DEMO_LABEL))
        } else {
            let ctx = ctx.clone();
            let wake = Arc::new(move || ctx.request_repaint());
            (Box::new(CoreSession::new(wake)), MEDIA_LABEL)
        };
        let (save_sender, save_results) = channel();
        #[cfg(windows)]
        let global_keys = GlobalKeys::new(ctx);
        #[cfg(windows)]
        let tray = Tray::new(ctx);
        Self {
            state: AppState::new(settings),
            session,
            settings_path,
            banner,
            renderer: views::share::VideoRenderer::default(),
            images: ChatImages::default(),
            save_results,
            save_sender,
            #[cfg(windows)]
            global_keys,
            #[cfg(windows)]
            tray,
            quitting: false,
            fullscreen: false,
        }
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

    fn pump_session(&mut self) {
        while let Some(event) = self.session.try_recv() {
            self.state.apply_event(event);
        }
        if let Some(command) = self.state.poll_refresh(Instant::now()) {
            self.session.send(command);
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

    fn prune_images(&mut self) {
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
                self.images.retain(|id| ids.contains(id));
            }
            _ => self.images.retain(|_| false),
        }
    }

    fn dispatch(&mut self, actions: Vec<UserAction>) {
        if actions.contains(&UserAction::SubmitLogin) && self.state.can_submit_login() {
            self.session.configure(self.state.credentials());
        }
        let commands: Vec<UiCommand> = actions
            .into_iter()
            .flat_map(|action| self.state.handle_action(action))
            .collect();
        for command in commands {
            self.session.send(command);
        }
        if let (Some(settings), Some(path)) =
            (self.state.take_pending_save(), self.settings_path.clone())
        {
            settings::save_in_background(path, settings);
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_session();
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
                            self.dispatch(vec![UserAction::HotkeyPressed(action)]);
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
                    TrayAction::Open => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    TrayAction::ToggleMute => {
                        self.dispatch(vec![UserAction::ToggleMute]);
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
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump_saves();
        if let Some(label) = self.banner {
            views::screens::demo_banner(ui, label);
        }

        let ptt_hint = self.state.push_to_talk_hint();
        let AppState {
            screen,
            form,
            rooms,
            audio,
            hotkeys,
            close_to_tray,
            picker,
            ..
        } = &mut self.state;
        let renderer = &mut self.renderer;
        let images = &mut self.images;
        let mut actions = match screen {
            Screen::Login => views::login::show(ui, form),
            Screen::Connecting { room } => views::screens::connecting(ui, room),
            Screen::InRoom(room) => views::room::show(
                ui,
                room,
                rooms,
                server_host(&form.server_url),
                renderer,
                images,
                audio.stream_volume,
                ptt_hint.as_deref(),
            ),
            Screen::Error { message } => views::screens::error(ui, message),
        };
        actions.extend(views::settings::show(
            ui.ctx(),
            audio,
            hotkeys,
            *close_to_tray,
        ));
        actions.extend(views::share::picker(ui.ctx(), picker));
        let actions = self.resolve_effects(ui.ctx(), actions);
        self.dispatch(actions);
        self.prune_images();

        if ui.ctx().input(|i| i.viewport().close_requested())
            && self.state.close_to_tray
            && self.has_tray()
            && !self.quitting
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        let fullscreen = self.state.is_fullscreen();
        if fullscreen != self.fullscreen {
            self.fullscreen = fullscreen;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
        }
        let interval = if self.state.is_watching() {
            VIDEO_REPAINT_INTERVAL
        } else {
            REPAINT_INTERVAL
        };
        ui.ctx().request_repaint_after(interval);
    }
}
