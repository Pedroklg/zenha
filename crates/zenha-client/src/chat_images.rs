//! Miniaturas das imagens do chat: decodificadas fora da thread da interface.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use image::{ImageReader, Limits};
use zenha_protocol::MAX_FILE_BYTES;

const MAX_ENTRIES: usize = 64;
const MAX_SIDE: u32 = 8192;
const MAX_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
const THUMB_WIDTH: u32 = 480;
const THUMB_HEIGHT: u32 = 360;
pub const VIEWER_MAX_TEXTURE_SIDE: u32 = 4096;

#[derive(Clone)]
pub enum ThumbState {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

enum Slot {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

type Decoded = (String, Result<egui::ColorImage, ()>);

pub struct ChatImages {
    slots: HashMap<String, Slot>,
    order: VecDeque<String>,
    sender: Sender<Decoded>,
    receiver: Receiver<Decoded>,
}

impl Default for ChatImages {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            slots: HashMap::new(),
            order: VecDeque::new(),
            sender,
            receiver,
        }
    }
}

impl ChatImages {
    pub fn texture(&mut self, ctx: &egui::Context, id: &str, path: &Path) -> ThumbState {
        self.drain(ctx);
        match self.slots.get(id) {
            Some(Slot::Loading) => ThumbState::Loading,
            Some(Slot::Ready(texture)) => ThumbState::Ready(texture.clone()),
            Some(Slot::Failed) => ThumbState::Failed,
            None => {
                self.start(ctx, id, path);
                match self.slots.get(id) {
                    Some(Slot::Failed) => ThumbState::Failed,
                    _ => ThumbState::Loading,
                }
            }
        }
    }

    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.slots.retain(|id, _| keep(id));
        self.order.retain(|id| self.slots.contains_key(id));
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok((id, result)) = self.receiver.try_recv() {
            let Some(slot) = self.slots.get_mut(&id) else {
                continue;
            };
            *slot = match result {
                Ok(image) => Slot::Ready(ctx.load_texture(
                    format!("chat-image-{id}"),
                    image,
                    egui::TextureOptions::LINEAR,
                )),
                Err(()) => Slot::Failed,
            };
        }
    }

    fn start(&mut self, ctx: &egui::Context, id: &str, path: &Path) {
        while self.order.len() >= MAX_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.slots.remove(&oldest);
            }
        }
        self.order.push_back(id.to_owned());
        self.slots.insert(id.to_owned(), Slot::Loading);

        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let id = id.to_owned();
        let path: PathBuf = path.to_owned();
        let task_id = id.clone();
        let spawned = std::thread::Builder::new()
            .name("chat-thumbnail".to_owned())
            .spawn(move || {
                let result = decode_thumbnail(&path)
                    .map(|image| {
                        let size = [image.width() as usize, image.height() as usize];
                        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw())
                    })
                    .map_err(|err| {
                        tracing::debug!(%err, path = %path.display(), "could not decode chat image");
                    });
                if sender.send((task_id, result)).is_ok() {
                    ctx.request_repaint();
                }
            });
        if let Err(err) = spawned {
            tracing::debug!(%err, "could not start thumbnail thread");
            self.slots.insert(id, Slot::Failed);
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum DecodeError {
    #[error("file is too large ({0} bytes)")]
    TooLarge(u64),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Image(#[from] image::ImageError),
}

type ViewerDecoded = (u64, String, Result<egui::ColorImage, ()>);

struct ViewerEntry {
    id: String,
    seq: u64,
    slot: Slot,
}

/// Cache de textura única para a imagem aberta no visualizador modal do chat.
pub struct ViewerImage {
    current: Option<ViewerEntry>,
    next_seq: u64,
    sender: Sender<ViewerDecoded>,
    receiver: Receiver<ViewerDecoded>,
}

impl Default for ViewerImage {
    fn default() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self {
            current: None,
            next_seq: 0,
            sender,
            receiver,
        }
    }
}

impl ViewerImage {
    /// Retorna o estado da imagem em tamanho grande, decodificando em segundo
    /// plano quando necessário e mantendo apenas a imagem atualmente aberta.
    pub fn texture(&mut self, ctx: &egui::Context, id: &str, path: &Path) -> ThumbState {
        self.drain(ctx);
        if let Some(entry) = self.current.as_ref().filter(|entry| entry.id == id) {
            return slot_state(&entry.slot);
        }
        self.start(ctx, id, path)
    }

    /// Mantém apenas a textura correspondente a `open_id`, liberando a anterior
    /// assim que o visualizador fecha ou muda de imagem.
    pub fn retain_id(&mut self, open_id: Option<&str>) {
        match open_id {
            None => {
                // Drain finished background decodes when closed so no buffer lingers in the channel.
                while self.receiver.try_recv().is_ok() {}
                self.current = None;
            }
            Some(expected) => {
                if self
                    .current
                    .as_ref()
                    .is_some_and(|entry| entry.id != expected)
                {
                    self.current = None;
                }
            }
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.current.is_none()
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok((seq, id, decoded)) = self.receiver.try_recv() {
            let Some(entry) = &mut self.current else {
                continue;
            };
            if entry.seq != seq || entry.id != id {
                continue;
            }
            entry.slot = match decoded {
                Ok(image) => {
                    let texture = ctx.load_texture(
                        format!("chat-viewer-{id}-{seq}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    Slot::Ready(texture)
                }
                Err(()) => Slot::Failed,
            };
        }
    }

    fn start(&mut self, ctx: &egui::Context, id: &str, path: &Path) -> ThumbState {
        self.next_seq = self.next_seq.wrapping_add(1);
        let seq = self.next_seq;
        let max_side = viewer_max_side(ctx);
        let path_owned = path.to_path_buf();
        let id_owned = id.to_owned();
        let sender = self.sender.clone();
        let ctx = ctx.clone();

        let spawned = std::thread::Builder::new()
            .name("chat-image-viewer".to_owned())
            .spawn(move || {
                let result = decode_viewer_image(&path_owned, max_side).map_err(|err| {
                    tracing::warn!(
                        error = ?err,
                        path = %path_owned.display(),
                        "failed to decode chat image for viewer"
                    );
                });
                let _ = sender.send((seq, id_owned, result));
                ctx.request_repaint();
            });

        let slot = match spawned {
            Ok(_) => Slot::Loading,
            Err(err) => {
                tracing::warn!(error = %err, "failed to spawn viewer image decoder thread");
                Slot::Failed
            }
        };
        let state = slot_state(&slot);
        self.current = Some(ViewerEntry {
            id: id.to_owned(),
            seq,
            slot,
        });
        state
    }
}

fn slot_state(slot: &Slot) -> ThumbState {
    match slot {
        Slot::Loading => ThumbState::Loading,
        Slot::Ready(texture) => ThumbState::Ready(texture.clone()),
        Slot::Failed => ThumbState::Failed,
    }
}

fn viewer_max_side(ctx: &egui::Context) -> u32 {
    let reported = ctx.input(|input| input.max_texture_side);
    if reported == 0 {
        VIEWER_MAX_TEXTURE_SIDE
    } else {
        u32::try_from(reported)
            .unwrap_or(VIEWER_MAX_TEXTURE_SIDE)
            .min(VIEWER_MAX_TEXTURE_SIDE)
    }
}

fn decode_bounded(path: &Path) -> Result<image::DynamicImage, DecodeError> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_FILE_BYTES {
        return Err(DecodeError::TooLarge(size));
    }
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC_BYTES);
    reader.limits(limits);
    Ok(reader.decode()?)
}

fn decode_thumbnail(path: &Path) -> Result<image::RgbaImage, DecodeError> {
    let decoded = decode_bounded(path)?;
    if decoded.width() <= THUMB_WIDTH && decoded.height() <= THUMB_HEIGHT {
        return Ok(decoded.to_rgba8());
    }
    Ok(decoded.thumbnail(THUMB_WIDTH, THUMB_HEIGHT).to_rgba8())
}

fn decode_viewer_image(path: &Path, max_side: u32) -> Result<egui::ColorImage, DecodeError> {
    let decoded = decode_bounded(path)?;
    let max_side = max_side.clamp(1, VIEWER_MAX_TEXTURE_SIDE);
    let image = if decoded.width() <= max_side && decoded.height() <= max_side {
        decoded.to_rgba8()
    } else {
        decoded
            .resize(max_side, max_side, image::imageops::FilterType::Triangle)
            .to_rgba8()
    };
    Ok(rgba_to_color_image(image))
}

fn rgba_to_color_image(image: image::RgbaImage) -> egui::ColorImage {
    let size = [image.width() as usize, image.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw())
}

pub fn pending_paste_thumb_id(path: &Path) -> String {
    format!("pending-paste:{}", path.display())
}

pub fn fit_within(width: f32, height: f32, max_width: f32, max_height: f32) -> (f32, f32) {
    if width <= 0.0 || height <= 0.0 || max_width <= 0.0 || max_height <= 0.0 {
        return (0.0, 0.0);
    }
    let scale = (max_width / width).min(max_height / height).min(1.0);
    (width * scale, height * scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_file(extension: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "zenha-chat-images-test-{}-{n}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn fit_within_never_enlarges_and_keeps_ratio() {
        assert_eq!(fit_within(100.0, 50.0, 240.0, 180.0), (100.0, 50.0));
        assert_eq!(fit_within(480.0, 360.0, 240.0, 180.0), (240.0, 180.0));
        assert_eq!(fit_within(480.0, 120.0, 240.0, 180.0), (240.0, 60.0));
        assert_eq!(fit_within(100.0, 400.0, 240.0, 180.0), (45.0, 180.0));
    }

    #[test]
    fn fit_within_handles_zero() {
        assert_eq!(fit_within(0.0, 10.0, 240.0, 180.0), (0.0, 0.0));
        assert_eq!(fit_within(10.0, 0.0, 240.0, 180.0), (0.0, 0.0));
        assert_eq!(fit_within(10.0, 10.0, 0.0, 180.0), (0.0, 0.0));
    }

    #[test]
    fn decodes_and_shrinks_a_large_png() {
        let path = temp_file("png");
        let image = image::RgbaImage::from_pixel(960, 720, image::Rgba([10, 20, 30, 255]));
        image.save(&path).expect("write png");
        let thumb = decode_thumbnail(&path);
        let _ = std::fs::remove_file(&path);
        let thumb = thumb.expect("decode");
        assert_eq!((thumb.width(), thumb.height()), (480, 360));
    }

    #[test]
    fn keeps_a_small_png_size() {
        let path = temp_file("png");
        let image = image::RgbaImage::from_pixel(40, 20, image::Rgba([1, 2, 3, 255]));
        image.save(&path).expect("write png");
        let thumb = decode_thumbnail(&path);
        let _ = std::fs::remove_file(&path);
        let thumb = thumb.expect("decode");
        assert_eq!((thumb.width(), thumb.height()), (40, 20));
    }

    #[test]
    fn garbage_with_image_extension_is_an_error() {
        let path = temp_file("png");
        let junk: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(&path, junk).expect("write junk");
        let result = decode_thumbnail(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(decode_thumbnail(&temp_file("png")).is_err());
    }

    #[test]
    fn retain_drops_unlisted_ids() {
        let mut images = ChatImages::default();
        for id in ["a", "b"] {
            images.slots.insert(id.to_owned(), Slot::Failed);
            images.order.push_back(id.to_owned());
        }
        images.retain(|id| id == "b");
        assert_eq!(images.slots.len(), 1);
        assert_eq!(images.order, VecDeque::from(["b".to_owned()]));
        images.retain(|_| false);
        assert!(images.is_empty());
    }

    #[test]
    fn viewer_decodes_keeps_medium_and_small_png_sizes() {
        for (width, height) in [(960, 720), (40, 20)] {
            let path = temp_file("png");
            let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
            image.save(&path).expect("write png");
            let decoded = decode_viewer_image(&path, VIEWER_MAX_TEXTURE_SIDE);
            let _ = std::fs::remove_file(&path);
            let decoded = decoded.expect("decode");
            assert_eq!(decoded.size, [width as usize, height as usize]);
        }
    }

    #[test]
    fn viewer_decodes_shrinks_image_above_max_side_keeping_ratio() {
        let path = temp_file("png");
        let image = image::RgbaImage::from_pixel(200, 100, image::Rgba([40, 50, 60, 255]));
        image.save(&path).expect("write png");
        let decoded = decode_viewer_image(&path, 80);
        let _ = std::fs::remove_file(&path);
        let decoded = decoded.expect("decode");
        assert_eq!(decoded.size, [80, 40]);
    }

    #[test]
    fn viewer_decodes_rejects_corrupt_and_missing_files() {
        assert!(decode_viewer_image(&temp_file("png"), VIEWER_MAX_TEXTURE_SIDE).is_err());
        let path = temp_file("png");
        std::fs::write(&path, [0xdeu8, 0xad, 0xbe, 0xef]).expect("write junk");
        let decoded = decode_viewer_image(&path, VIEWER_MAX_TEXTURE_SIDE);
        let _ = std::fs::remove_file(&path);
        assert!(decoded.is_err());
    }

    #[test]
    fn viewer_image_retain_drops_closed_and_switched_entries() {
        let mut cache = ViewerImage {
            current: Some(ViewerEntry {
                id: "img-1".to_owned(),
                seq: 1,
                slot: Slot::Failed,
            }),
            ..ViewerImage::default()
        };
        assert!(!cache.is_empty());
        cache.retain_id(Some("img-1"));
        assert!(!cache.is_empty());
        cache.retain_id(Some("img-2"));
        assert!(cache.is_empty());

        cache.current = Some(ViewerEntry {
            id: "img-2".to_owned(),
            seq: 2,
            slot: Slot::Failed,
        });
        let _ = cache.sender.send((2, "img-2".to_owned(), Err(())));
        cache.retain_id(None);
        assert!(cache.is_empty());
        assert!(cache.receiver.try_recv().is_err());
    }
}
