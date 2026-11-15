use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, CornerRadius, Margin, RichText, Sense, Stroke, Vec2};

use crate::state::{SharePicker, UserAction, WatchState, aspect_fit};
use crate::theme;
use crate::ui_model::{CaptureTargetView, FeedHandle, SharePresetView};

const THUMBNAIL_CARD_WIDTH: f32 = 176.0;
const THUMBNAIL_PREVIEW_SIZE: Vec2 = Vec2::new(164.0, 92.0);
const THUMBNAIL_GRID_COLS: usize = 3;
const THUMBNAIL_SPINNER_TIMEOUT: Duration = Duration::from_millis(1300);

#[derive(Default)]
pub struct ThumbnailCache {
    serial: u64,
    refreshed_at: Option<Instant>,
    textures: HashMap<String, egui::TextureHandle>,
}

impl ThumbnailCache {
    fn sync(&mut self, ctx: &egui::Context, picker: &SharePicker) {
        if !picker.open {
            if !self.textures.is_empty() || self.refreshed_at.is_some() {
                self.textures.clear();
                self.refreshed_at = None;
            }
            self.serial = picker.targets_serial;
            return;
        }
        if self.serial != picker.targets_serial {
            self.serial = picker.targets_serial;
            self.textures.clear();
            self.refreshed_at = picker.loaded.then(Instant::now);
        } else if picker.loaded && self.refreshed_at.is_none() {
            self.refreshed_at = Some(Instant::now());
        }
        let max_side = ctx.input(|i| i.max_texture_side);
        for (id, thumb) in &picker.thumbnails {
            if self.textures.contains_key(id) {
                continue;
            }
            if thumb.width == 0
                || thumb.height == 0
                || thumb.width > max_side
                || thumb.height > max_side
                || thumb.rgba.len() != thumb.width * thumb.height * 4
            {
                continue;
            }
            let image =
                egui::ColorImage::from_rgba_unmultiplied([thumb.width, thumb.height], &thumb.rgba);
            let texture = ctx.load_texture(
                format!("capture-thumb:{id}"),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.textures.insert(id.clone(), texture);
        }
    }

    fn is_loading(&self, picker: &SharePicker) -> bool {
        picker.loaded
            && self
                .refreshed_at
                .is_some_and(|t| t.elapsed() < THUMBNAIL_SPINNER_TIMEOUT)
    }
}

#[derive(Default)]
pub struct VideoRenderer {
    texture: Option<egui::TextureHandle>,
    last_seen: u64,
    serial: u64,
    size: (usize, usize),
    paused: bool,
}

impl VideoRenderer {
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn reset(&mut self) {
        if self.texture.is_some() || self.last_seen != 0 || self.serial != 0 {
            let paused = self.paused;
            *self = Self {
                paused,
                ..Self::default()
            };
        }
    }

    fn sync(&mut self, ctx: &egui::Context, serial: u64, feed: &FeedHandle) {
        if self.serial != serial {
            *self = Self {
                serial,
                paused: self.paused,
                ..Self::default()
            };
        }
        let Some(frame) = feed.take_newer(&mut self.last_seen) else {
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
        self.show_feed(ui, watch.serial, &watch.feed, "Aguardando vídeo...");
    }

    pub fn show_feed(
        &mut self,
        ui: &mut egui::Ui,
        serial: u64,
        feed: &FeedHandle,
        waiting_label: &str,
    ) {
        if !self.paused {
            self.sync(ui.ctx(), serial, feed);
        }
        let avail = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(avail, Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme::VIDEO_BG);
        let Some(texture) = &self.texture else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                waiting_label,
                egui::FontId::proportional(14.0),
                theme::TEXT_WEAK,
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

pub fn picker(
    ctx: &egui::Context,
    picker: &SharePicker,
    cache: &mut ThumbnailCache,
) -> Vec<UserAction> {
    cache.sync(ctx, picker);
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
        .show(ctx, |ui| content(ui, picker, cache, &mut actions));
    if !open {
        actions.push(UserAction::CloseSharePicker);
    }
    actions
}

fn content(
    ui: &mut egui::Ui,
    picker: &SharePicker,
    cache: &ThumbnailCache,
    actions: &mut Vec<UserAction>,
) {
    let min_width = THUMBNAIL_CARD_WIDTH * THUMBNAIL_GRID_COLS as f32 + 32.0;
    ui.set_min_width(min_width);
    if !picker.loaded {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.weak("Procurando telas e janelas...");
        });
    } else if picker.targets.is_empty() {
        ui.weak("Nenhuma tela ou janela encontrada.");
    }
    let loading = cache.is_loading(picker);
    if loading
        && picker
            .targets
            .iter()
            .any(|t| !cache.textures.contains_key(&t.id))
    {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            group(
                ui,
                "Telas",
                picker.screens(),
                picker,
                cache,
                loading,
                actions,
            );
            group(
                ui,
                "Janelas",
                picker.windows(),
                picker,
                cache,
                loading,
                actions,
            );
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
    cache: &ThumbnailCache,
    loading: bool,
    actions: &mut Vec<UserAction>,
) {
    let targets: Vec<_> = targets.collect();
    if targets.is_empty() {
        return;
    }
    ui.strong(title);
    ui.add_space(4.0);
    for row in targets.chunks(THUMBNAIL_GRID_COLS) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for target in row {
                target_tile(ui, target, picker, cache, loading, actions);
            }
        });
        ui.add_space(8.0);
    }
}

fn target_tile(
    ui: &mut egui::Ui,
    target: &CaptureTargetView,
    picker: &SharePicker,
    cache: &ThumbnailCache,
    loading: bool,
    actions: &mut Vec<UserAction>,
) {
    let selected = picker.selected.as_deref() == Some(target.id.as_str());
    let (fill, stroke) = if selected {
        (theme::CONTROL_HOVER, Stroke::new(2.0, theme::ACCENT))
    } else {
        (theme::CONTROL, Stroke::new(1.0, theme::BORDER))
    };
    let card = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(THUMBNAIL_PREVIEW_SIZE.x);
            let (thumb_rect, _) = ui.allocate_exact_size(THUMBNAIL_PREVIEW_SIZE, Sense::hover());
            ui.painter()
                .rect_filled(thumb_rect, CornerRadius::same(4), theme::VIDEO_BG);
            if let Some(texture) = cache.textures.get(&target.id) {
                let size = texture.size_vec2();
                let (w, h) = aspect_fit(
                    (thumb_rect.width(), thumb_rect.height()),
                    (size.x as usize, size.y as usize),
                );
                let dst = egui::Rect::from_center_size(thumb_rect.center(), Vec2::new(w, h));
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter()
                    .image(texture.id(), dst, uv, egui::Color32::WHITE);
            } else if loading {
                ui.place(thumb_rect, egui::Spinner::new().size(20.0));
            } else {
                ui.painter().text(
                    thumb_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Sem prévia",
                    egui::FontId::proportional(11.0),
                    theme::TEXT_WEAK,
                );
            }
            ui.add_space(6.0);
            ui.add_sized(
                Vec2::new(THUMBNAIL_PREVIEW_SIZE.x, 18.0),
                egui::Label::new(RichText::new(&target.name).size(12.5).color(theme::TEXT))
                    .truncate()
                    .selectable(false),
            );
        });
    let response = ui.interact(
        card.response.rect,
        ui.make_persistent_id(("capture-target", &target.id)),
        Sense::click(),
    );
    if response.clicked() {
        actions.push(UserAction::SelectCaptureTarget(target.id.clone()));
    }
    response.on_hover_text(&target.name);
}
