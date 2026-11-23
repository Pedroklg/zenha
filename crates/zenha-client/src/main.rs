#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod avatars;
mod chat_images;
mod cli;
mod core_session;
mod demo;
mod dialogs;
mod format;
mod icons;
mod invite;
mod personal_hook;
mod platform;
mod state;
mod theme;
mod ui_model;
mod updater;
mod views;

use eframe::egui;

pub(crate) const APP_NAME: &str = "Zenha";

fn main() -> anyhow::Result<()> {
    let launch = cli::parse_args(std::env::args().skip(1));
    let log_path = platform::logging::init(launch.debug, !launch.demo && !launch.diagnostics);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        debug = launch.debug,
        demo = launch.demo,
        log = ?log_path,
        "zenha starting"
    );
    if launch.diagnostics {
        write_diagnostics(log_path.as_deref());
        return Ok(());
    }
    let result = run(launch);
    if let Err(err) = &result {
        tracing::error!("{err:#}");
        dialogs::show_error(&format!(
            "O Zenha não conseguiu abrir.\n\n{err:#}\n\nLog: {}",
            log_path
                .as_deref()
                .map_or_else(|| "indisponível".to_owned(), |p| p.display().to_string())
        ));
    }
    result
}

fn write_diagnostics(log_path: Option<&std::path::Path>) {
    let settings = platform::settings::default_path()
        .as_deref()
        .map(platform::settings::load)
        .unwrap_or_default();
    match platform::diagnostics::write_report(&settings, log_path) {
        Ok(path) => {
            tracing::info!(path = %path.display(), "diagnostics report written");
            dialogs::reveal_file(&path);
            dialogs::show_info(&format!(
                "Relatório salvo em:\n{}\n\nMande esse arquivo para quem cuida do servidor.",
                path.display()
            ));
        }
        Err(err) => {
            tracing::error!(%err, "could not write diagnostics report");
            dialogs::show_error(&format!("Não deu para salvar o relatório: {err}"));
        }
    }
}

fn run(launch: cli::LaunchArgs) -> anyhow::Result<()> {
    let demo_flag = launch.demo;
    let startup_link = launch.startup_link;
    #[cfg(windows)]
    let instance = if demo_flag {
        None
    } else {
        let raw_link = std::env::args().skip(1).find(|arg| !arg.trim().is_empty());
        match platform::single_instance::start(raw_link.as_deref()) {
            platform::single_instance::Startup::Run(listener) => listener,
            platform::single_instance::Startup::Exit => {
                tracing::info!("another instance is running; handed off and exiting");
                return Ok(());
            }
        }
    };
    let raw_settings_path = platform::settings::default_path();
    let mut settings = raw_settings_path
        .as_deref()
        .map(platform::settings::load)
        .unwrap_or_default();
    let (settings_path, saved_password) = if demo_flag {
        settings.client_id = demo::SELF_IDENTITY.to_owned();
        settings.display_name = demo::SELF_NAME.to_owned();
        settings.token_server_url = demo::DEMO_SERVER_URL.to_owned();
        // Keep --demo isolated from the user's real config file and Credential Manager.
        (None, Some(demo::DEMO_PASSWORD.to_owned()))
    } else {
        if platform::settings::ensure_client_id(&mut settings)
            && let Some(path) = raw_settings_path.as_deref()
            && let Err(err) = platform::settings::save(path, &settings)
        {
            tracing::warn!(%err, "could not save generated client id");
        }
        let server = settings.token_server_url.trim();
        let password = (!server.is_empty())
            .then(|| platform::credentials::load(server))
            .flatten();
        (raw_settings_path, password)
    };

    if let Err(err) = std::thread::Builder::new()
        .name("cleanup-temp-files".into())
        .spawn(|| {
            platform::clipboard_image::cleanup_old_pastes(
                &platform::clipboard_image::paste_temp_dir(),
            );
            updater::cleanup_update_dir(&updater::update_temp_dir());
            zenha_core::cleanup_orphan_received_dirs();
        })
    {
        tracing::debug!(%err, "could not start startup cleanup thread");
    }

    #[cfg_attr(not(windows), expect(unused_mut))]
    let mut options = eframe::NativeOptions {
        viewport: window_viewport(),
        ..Default::default()
    };
    // Vulkan/GL adapter enumeration can spin forever on old hybrid-GPU drivers; DX12 always has WARP.
    #[cfg(windows)]
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends =
            eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::DX12);
    }

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
