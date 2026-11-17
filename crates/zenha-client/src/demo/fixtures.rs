//! Dados fixos da demonstração: avatar, alvos de captura e dispositivos de áudio.

use crate::ui_model::{AudioDeviceView, CaptureTargetView, TargetKindView};

pub fn demo_avatar_image() -> eframe::egui::ColorImage {
    const SIDE: usize = 128;
    let pixels = (0..SIDE * SIDE)
        .map(|index| {
            let (x, y) = ((index % SIDE) as u8, (index / SIDE) as u8);
            eframe::egui::Color32::from_rgb(x.saturating_mul(2), y.saturating_mul(2), 180)
        })
        .collect();
    eframe::egui::ColorImage::new([SIDE, SIDE], pixels)
}

pub(super) fn demo_targets() -> Vec<CaptureTargetView> {
    let target = |id: &str, name: &str, kind| CaptureTargetView {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
    };
    vec![
        target(
            "demo:screen:0",
            "Tela 1 (principal)",
            TargetKindView::Screen,
        ),
        target("demo:screen:1", "Tela 2", TargetKindView::Screen),
        target(
            "demo:window:0",
            "Navegador - Demonstração",
            TargetKindView::Window,
        ),
        target("demo:window:1", "Editor de texto", TargetKindView::Window),
    ]
}

pub(super) fn demo_devices(kind: &str, names: &[&str]) -> Vec<AudioDeviceView> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| AudioDeviceView {
            id: format!("demo-{kind}-{index}"),
            name: (*name).to_owned(),
        })
        .collect()
}
