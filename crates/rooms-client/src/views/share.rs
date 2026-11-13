use eframe::egui;

use crate::state::{SharePicker, UserAction, WatchState, aspect_fit};
use crate::ui_model::{CaptureTargetView, SharePresetView};

#[derive(Default)]
pub struct VideoRenderer {
    texture: Option<egui::TextureHandle>,
    last_seen: u64,
    serial: u64,
    size: (usize, usize),
}

impl VideoRenderer {
    fn sync(&mut self, ctx: &egui::Context, watch: &WatchState) {
        if self.serial != watch.serial {
            *self = Self {
                serial: watch.serial,
                ..Self::default()
            };
        }
        let Some(frame) = watch.feed.take_newer(&mut self.last_seen) else {
            return;
        };
        let max_side = ctx.input(|i| i.max_texture_side);
        let bytes = frame.bytes();
        let valid = frame.width > 0
            && frame.height > 0
            && frame.width <= max_side
            && frame.height <= max_side
            && bytes.len() == frame.width * frame.height * 4;
        if !valid {
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], bytes);
        match &mut self.texture {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("screen-share", image, egui::TextureOptions::LINEAR));
            }
        }
        self.size = (frame.width, frame.height);
    }

    pub fn show(&mut self, ui: &mut egui::Ui, watch: &WatchState) {
        self.sync(ui.ctx(), watch);
        let avail = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
        let Some(texture) = &self.texture else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Aguardando vídeo...",
                egui::FontId::proportional(16.0),
                egui::Color32::GRAY,
            );
            return;
        };
        let (w, h) = aspect_fit((rect.width(), rect.height()), self.size);
        let target = egui::Rect::from_center_size(rect.center(), egui::vec2(w, h));
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter()
            .image(texture.id(), target, uv, egui::Color32::WHITE);
    }
}

pub fn picker(ctx: &egui::Context, picker: &SharePicker) -> Vec<UserAction> {
    let mut actions = Vec::new();
    if !picker.open {
        return actions;
    }
    let mut open = true;
    egui::Window::new("Transmitir tela")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| content(ui, picker, &mut actions));
    if !open {
        actions.push(UserAction::CloseSharePicker);
    }
    actions
}

fn content(ui: &mut egui::Ui, picker: &SharePicker, actions: &mut Vec<UserAction>) {
    ui.set_min_width(360.0);
    if !picker.loaded {
        ui.weak("Procurando telas e janelas...");
    } else if picker.targets.is_empty() {
        ui.weak("Nenhuma tela ou janela encontrada.");
    }
    egui::ScrollArea::vertical()
        .max_height(260.0)
        .show(ui, |ui| {
            group(ui, "Telas", picker.screens(), picker, actions);
            group(ui, "Janelas", picker.windows(), picker, actions);
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Qualidade");
        egui::ComboBox::from_id_salt("share_preset")
            .selected_text(picker.preset.label())
            .show_ui(ui, |ui| {
                for preset in SharePresetView::ALL {
                    if ui
                        .selectable_label(picker.preset == preset, preset.label())
                        .clicked()
                    {
                        actions.push(UserAction::SelectSharePreset(preset));
                    }
                }
            });
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(picker.can_start(), egui::Button::new("Iniciar"))
            .clicked()
        {
            actions.push(UserAction::StartShare);
        }
        if ui.button("Atualizar").clicked() {
            actions.push(UserAction::RefreshCaptureTargets);
        }
        if ui.button("Cancelar").clicked() {
            actions.push(UserAction::CloseSharePicker);
        }
    });
}

fn group<'a>(
    ui: &mut egui::Ui,
    title: &str,
    targets: impl Iterator<Item = &'a CaptureTargetView>,
    picker: &SharePicker,
    actions: &mut Vec<UserAction>,
) {
    let targets: Vec<_> = targets.collect();
    if targets.is_empty() {
        return;
    }
    ui.strong(title);
    for target in targets {
        let selected = picker.selected.as_deref() == Some(target.id.as_str());
        if ui.selectable_label(selected, &target.name).clicked() {
            actions.push(UserAction::SelectCaptureTarget(target.id.clone()));
        }
    }
    ui.add_space(6.0);
}
