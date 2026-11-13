#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chat_images;
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
mod state;
mod theme;
mod tray;
mod ui_model;
mod views;

use eframe::egui;

pub(crate) const APP_NAME: &str = "Zenha";

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let demo_flag = std::env::args().skip(1).any(|arg| arg == "--demo");
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
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([640.0, 400.0]),
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
            )))
        }),
    )
    .map_err(|err| anyhow::anyhow!("could not start the window: {err}"))
}
