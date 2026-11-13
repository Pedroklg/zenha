#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chat_images;
mod core_session;
mod demo;
mod dialogs;
mod format;
#[cfg(windows)]
mod global_keys;
mod hotkeys;
mod icons;
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
    let settings_path = settings::default_path();
    let settings = settings_path
        .as_deref()
        .map(settings::load)
        .unwrap_or_default();

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
                settings_path,
                demo_flag,
            )))
        }),
    )
    .map_err(|err| anyhow::anyhow!("could not start the window: {err}"))
}
