#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chat_images;
mod cli;
mod core_session;
mod credentials;
mod demo;
mod dialogs;
mod format;
#[cfg(windows)]
mod global_keys;
mod hotkeys;
mod icons;
mod invite;
mod settings;
#[cfg(windows)]
mod single_instance;
mod state;
mod theme;
mod tray;
mod ui_model;
mod update;
mod views;

use eframe::egui;

pub(crate) const APP_NAME: &str = "Zenha";

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let launch = cli::parse_args(std::env::args().skip(1));
    let demo_flag = launch.demo;
    let startup_link = launch.startup_link;
    #[cfg(windows)]
    let instance = if demo_flag {
        None
    } else {
        let raw_link = std::env::args().skip(1).find(|arg| !arg.trim().is_empty());
        match single_instance::start(raw_link.as_deref()) {
            single_instance::Startup::Run(listener) => listener,
            single_instance::Startup::Exit => return Ok(()),
        }
    };
    let raw_settings_path = settings::default_path();
    let mut settings = raw_settings_path
        .as_deref()
        .map(settings::load)
        .unwrap_or_default();
    let (settings_path, saved_password) = if demo_flag {
        settings.client_id = demo::SELF_IDENTITY.to_owned();
        settings.display_name = demo::SELF_NAME.to_owned();
        settings.token_server_url = demo::DEMO_SERVER_URL.to_owned();
        // Keep --demo isolated from the user's real config file and Credential Manager.
        (None, Some(demo::DEMO_PASSWORD.to_owned()))
    } else {
        if settings::ensure_client_id(&mut settings)
            && let Some(path) = raw_settings_path.as_deref()
            && let Err(err) = settings::save(path, &settings)
        {
            tracing::warn!(%err, "could not save generated client id");
        }
        let server = settings.token_server_url.trim();
        let password = (!server.is_empty())
            .then(|| credentials::load(server))
            .flatten();
        (raw_settings_path, password)
    };

    let options = eframe::NativeOptions {
        viewport: window_viewport(),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                &cc.egui_ctx,
                &settings,
                saved_password,
                settings_path,
                demo_flag,
                startup_link,
                #[cfg(windows)]
                instance,
            )))
        }),
    )
    .map_err(|err| anyhow::anyhow!("could not start the window: {err}"))
}

fn window_viewport() -> egui::ViewportBuilder {
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([960.0, 600.0])
        .with_min_inner_size([640.0, 400.0]);
    match eframe::icon_data::from_png_bytes(theme::APP_ICON_PNG) {
        Ok(icon) => viewport.with_icon(icon),
        Err(err) => {
            tracing::warn!(%err, "could not decode window icon");
            viewport
        }
    }
}
