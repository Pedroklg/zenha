//! Miniaturas das imagens do chat: decodificadas fora da thread da interface.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use image::{ImageReader, Limits};
use rooms_protocol::MAX_FILE_BYTES;

const MAX_ENTRIES: usize = 64;
const MAX_SIDE: u32 = 8192;
const MAX_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
const THUMB_WIDTH: u32 = 480;
const THUMB_HEIGHT: u32 = 360;

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

fn decode_thumbnail(path: &Path) -> Result<image::RgbaImage, DecodeError> {
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
    let decoded = reader.decode()?;
    if decoded.width() <= THUMB_WIDTH && decoded.height() <= THUMB_HEIGHT {
        return Ok(decoded.to_rgba8());
    }
    Ok(decoded.thumbnail(THUMB_WIDTH, THUMB_HEIGHT).to_rgba8())
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
}
