//! `eframe` application: `App` state and creation; the rest lives in the child modules.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::avatars::Avatars;
use crate::chat_images::{ChatImages, ViewerImage};
use crate::cli::StartupLink;
use crate::core_session::CoreSession;
use crate::demo::DemoSession;
use crate::platform::clipboard_image::{PasteImageError, SavedPastedImage};
#[cfg(windows)]
use crate::platform::global_keys::GlobalKeys;
use crate::platform::settings::Settings;
#[cfg(windows)]
use crate::platform::single_instance::Listener;
#[cfg(windows)]
use crate::platform::tray::Tray;
use crate::state::{AppState, Screen};
use crate::theme;
use crate::ui_model::SessionPort;
use crate::updater::{self, AvailableUpdate, UpdateDownloadEvent};
use crate::views;

mod effects;
mod frame;
mod pump;
#[cfg(test)]
mod tests;
mod window;

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
    avatars: Avatars,
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
    active_download: Option<updater::DownloadHandle>,
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
        let avatar_cache_dir = settings_path
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| dir.join("avatars"));
        state.set_avatar_cache_dir(avatar_cache_dir.clone());
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
            state.set_available_update(updater::demo_available_update());
            None
        } else {
            state.set_can_auto_update(updater::is_installed_exe());
            updater::spawn_check(ctx.clone())
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
            avatars: Avatars::default(),
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
        if demo_flag {
            app.avatars.register_image(
                ctx,
                crate::demo::DEMO_AVATAR_IDENTITY,
                crate::demo::DEMO_AVATAR_VERSION,
                crate::demo::demo_avatar_image(),
            );
        } else if let (Some(dir), Some(version)) = (avatar_cache_dir, &app.state.avatar_version) {
            let path = dir.join(format!("{}-{version}.png", app.state.client_id()));
            let identity = app.state.client_id().to_owned();
            app.avatars.register(ctx, &identity, version, path);
        }
        app.flush_state_effects(ctx);
        app
    }
}
