//! `App` tests with `egui_kittest`.

use std::sync::mpsc::channel;
use std::time::Duration;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::App;
use super::effects::open_path_in_os;
use super::window::window_is_hidden;
use crate::cli::StartupLink;
use crate::demo;
use crate::platform::settings::Settings;
use crate::state::{Screen, UserAction};

fn demo_settings(display_name: &str) -> Settings {
    Settings {
        client_id: demo::SELF_IDENTITY.to_owned(),
        display_name: display_name.to_owned(),
        token_server_url: demo::DEMO_SERVER_URL.to_owned(),
        ..Settings::default()
    }
}

fn demo_harness_with_settings(size: egui::Vec2, settings: Settings) -> Harness<'static, App> {
    Harness::builder()
        .with_size(size)
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
        })
}

fn demo_harness(size: egui::Vec2, display_name: &str) -> Harness<'static, App> {
    demo_harness_with_settings(size, demo_settings(display_name))
}

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
        let mut harness = demo_harness(egui::Vec2::new(960.0, 600.0), demo::SELF_NAME);

        harness.run_steps(2);
        assert!(matches!(harness.state().state.screen, Screen::Lobby));
        assert_eq!(harness.state().state.rooms.len(), 5);

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
    let mut harness = demo_harness_with_settings(
        egui::Vec2::new(1280.0, 720.0),
        Settings {
            noise_suppression: crate::platform::settings::NoiseMode::Enhanced,
            noise_gate_db: Some(-45),
            ..demo_settings(demo::SELF_NAME)
        },
    );

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
    let mut harness = demo_harness(egui::Vec2::new(1280.0, 720.0), demo::SELF_NAME);

    harness.run_steps(2);
    harness.get_by_label("Geral").click();
    harness.run_steps(2);

    let dir = std::env::temp_dir().join(format!("zenha-kittest-paste-{}", std::process::id()));
    let image = egui::ColorImage::new([2, 2], vec![egui::Color32::RED; 4]);
    let saved = crate::platform::clipboard_image::save_pasted_image(&image, &dir).unwrap();
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

    let saved_enter = crate::platform::clipboard_image::save_pasted_image(&image, &dir).unwrap();
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
    let mut harness = demo_harness(egui::Vec2::new(1280.0, 720.0), demo::SELF_NAME);

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
    let mut harness = demo_harness(egui::Vec2::new(960.0, 600.0), demo::SELF_NAME);

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

#[test]
fn room_ui_footer_chat_and_participant_card_layout_at_1280x720() {
    let long_name = "NomeMuitoLongoParaRodapeDaBarra";
    let mut harness = demo_harness(egui::Vec2::new(1280.0, 720.0), long_name);

    harness.run_steps(2);
    let assert_footer_fits = |harness: &Harness<'_, App>, status_label: &str| {
        let dot = harness.get_by_label(status_label).rect();
        let name = harness
            .query_all_by_label(long_name)
            .map(|node| node.rect())
            .find(|rect| rect.left() < 256.0 && (rect.center().y - dot.center().y).abs() < 20.0)
            .unwrap_or_else(|| panic!("missing footer name {long_name:?} next to {dot:?}"));
        let mic = harness.get_by_label("Desligar microfone").rect();
        let deafen = harness.get_by_label("Desligar áudio").rect();
        let gear = harness.get_by_label("Configurações").rect();

        assert!(
            name.height() < 24.0,
            "footer name wrapped unexpectedly: {name:?}"
        );
        assert!(dot.right() <= name.left() + 1.0, "{dot:?} vs {name:?}");
        assert!(name.right() <= mic.left() + 1.0, "{name:?} vs {mic:?}");
        assert!(mic.right() <= deafen.left() + 1.0, "{mic:?} vs {deafen:?}");
        assert!(
            deafen.right() <= gear.left() + 1.0,
            "{deafen:?} vs {gear:?}"
        );
        assert!(gear.right() <= 256.0, "footer button pushed out: {gear:?}");
        assert!(!name.intersects(mic));
        assert!(!dot.intersects(name));
    };

    assert_footer_fits(&harness, "Fora de sala");

    harness.get_by_label("Geral").click();
    harness.run_steps(2);
    harness
        .state_mut()
        .state
        .apply_event(crate::ui_model::UiEvent::ParticipantUpdated(
            crate::ui_model::ParticipantView {
                identity: demo::SELF_IDENTITY.to_owned(),
                name: long_name.to_owned(),
                muted: false,
                speaking: false,
                avatar_version: None,
            },
        ));
    harness.run_steps(2);

    assert_footer_fits(&harness, "Conectado");

    let attach = harness.get_by_label("Anexar arquivo").rect();
    let chat_input = harness.get_by_role(egui::accesskit::Role::TextInput).rect();
    let temp_toggle = harness
        .get_by_label("Mensagem temporária (clique para salvar)")
        .rect();
    assert!(harness.query_by_label("Anexar").is_none());
    assert!(
        (attach.height() - chat_input.height()).abs() <= 1.0,
        "attach {attach:?} height differs from chat input {chat_input:?}"
    );
    assert!(
        attach.right() < chat_input.left(),
        "{attach:?} vs {chat_input:?}"
    );
    assert!(
        chat_input.right() < temp_toggle.left(),
        "{chat_input:?} vs {temp_toggle:?}"
    );

    let status = harness.get_by_label("Transmitindo").rect();
    let watch = harness.get_by_label("Assistir").rect();
    assert!(harness.query_by_label("Transmitindo a tela").is_none());
    assert!(
        status.height() < 24.0,
        "card status wrapped to multiple lines: {status:?}"
    );
    assert!(
        status.right() <= watch.left(),
        "status {status:?} overlaps watch {watch:?}"
    );
    assert!(
        watch.right() <= 1280.0 - 320.0,
        "watch button out of room panel bounds: {watch:?}"
    );
    assert!(!status.intersects(watch));
}

#[test]
fn settings_mic_meter_messages_for_lobby_waiting_and_muted_states() {
    let mut harness = demo_harness_with_settings(
        egui::Vec2::new(1280.0, 720.0),
        Settings {
            noise_suppression: crate::platform::settings::NoiseMode::Enhanced,
            ..demo_settings(demo::SELF_NAME)
        },
    );

    harness.run_steps(2);
    harness.get_by_label("Configurações").click();
    harness.run_steps(3);
    assert!(
        harness
            .query_by_label("Entre em uma sala para ver o nível do microfone.")
            .is_some()
    );

    let ctx = harness.ctx.clone();
    harness
        .state_mut()
        .dispatch(&ctx, vec![UserAction::CloseSettings]);
    harness.run_steps(2);
    harness.get_by_label("Geral").click();
    harness.run_steps(2);
    harness.get_by_label("Configurações").click();
    harness.run_steps(3);
    assert!(
        harness
            .query_by_label("Aguardando sinal do microfone…")
            .is_some()
    );

    harness
        .state_mut()
        .dispatch(&ctx, vec![UserAction::ToggleMute]);
    harness
        .state_mut()
        .state
        .apply_event(crate::ui_model::UiEvent::MicLevel {
            level_db: -38,
            gate_open: true,
        });
    harness.run_steps(2);
    assert!(harness.query_by_label("Microfone desligado.").is_some());
    assert!(harness.query_by_label("Nível do microfone").is_none());

    harness
        .state_mut()
        .dispatch(&ctx, vec![UserAction::ToggleMute]);
    harness.run_steps(2);
    assert!(harness.query_by_label("Microfone desligado.").is_none());
    assert!(harness.query_by_label("Nível do microfone").is_some());
    assert!(harness.query_by_label("-38 dB").is_some());
}
