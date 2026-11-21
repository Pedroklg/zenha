//! Profile photos: decoded off the UI thread and stored as textures in the egui context.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use image::{ImageReader, Limits};

const MAX_SIDE: u32 = 512;
const MAX_ALLOC_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CACHED_AVATARS: usize = 64;

#[derive(Clone)]
struct Stored {
    version: String,
    texture: egui::TextureHandle,
}

type Decoded = (String, String, egui::ColorImage);

pub struct Avatars {
    sender: Sender<Decoded>,
    receiver: Receiver<Decoded>,
    order: VecDeque<String>,
}

impl Default for Avatars {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            sender,
            receiver,
            order: VecDeque::new(),
        }
    }
}

impl Avatars {
    pub fn register(&mut self, ctx: &egui::Context, identity: &str, version: &str, path: PathBuf) {
        if texture(ctx, identity, Some(version)).is_some() {
            self.touch(identity);
            return;
        }
        self.spawn_decode(ctx, identity, version, path);
    }

    pub fn register_image(
        &mut self,
        ctx: &egui::Context,
        identity: &str,
        version: &str,
        image: egui::ColorImage,
    ) {
        self.store(ctx, identity, version, image);
    }

    pub fn drain(&mut self, ctx: &egui::Context) {
        while let Ok((identity, version, image)) = self.receiver.try_recv() {
            self.store(ctx, &identity, &version, image);
        }
    }

    pub fn clear_unused(&mut self, ctx: &egui::Context, mut keep: impl FnMut(&str) -> bool) {
        let mut removed = Vec::new();
        self.order.retain(|id| {
            if keep(id) {
                true
            } else {
                removed.push(id.clone());
                false
            }
        });
        if !removed.is_empty() {
            ctx.data_mut(|data| {
                for id in &removed {
                    data.remove::<Stored>(key(id));
                }
            });
        }
    }

    fn touch(&mut self, identity: &str) {
        if let Some(pos) = self.order.iter().position(|id| id == identity) {
            self.order.remove(pos);
        }
        self.order.push_back(identity.to_owned());
    }

    fn store(
        &mut self,
        ctx: &egui::Context,
        identity: &str,
        version: &str,
        image: egui::ColorImage,
    ) {
        let texture = ctx.load_texture(
            format!("avatar-{identity}-{version}"),
            image,
            egui::TextureOptions::LINEAR,
        );
        let stored = Stored {
            version: version.to_owned(),
            texture,
        };
        ctx.data_mut(|data| data.insert_temp(key(identity), stored));
        self.touch(identity);
        while self.order.len() > MAX_CACHED_AVATARS
            && let Some(evicted) = self.order.pop_front()
        {
            ctx.data_mut(|data| data.remove::<Stored>(key(&evicted)));
        }
    }

    fn spawn_decode(&self, ctx: &egui::Context, identity: &str, version: &str, path: PathBuf) {
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let identity = identity.to_owned();
        let version = version.to_owned();
        let spawned = std::thread::Builder::new()
            .name("avatar-decode".to_owned())
            .spawn(move || match decode(&path) {
                Ok(image) => {
                    if sender.send((identity, version, image)).is_ok() {
                        ctx.request_repaint();
                    }
                }
                Err(err) => {
                    tracing::debug!(%err, path = %path.display(), "could not decode avatar");
                }
            });
        if let Err(err) = spawned {
            tracing::debug!(%err, "could not start avatar thread");
        }
    }
}

fn key(identity: &str) -> egui::Id {
    egui::Id::unique(("zenha-avatar", identity))
}

/// Texture of the identity's photo, only when it matches the wanted version.
pub fn texture(
    ctx: &egui::Context,
    identity: &str,
    version: Option<&str>,
) -> Option<egui::TextureHandle> {
    let version = version?;
    ctx.data(|data| data.get_temp::<Stored>(key(identity)))
        .filter(|stored| stored.version == version)
        .map(|stored| stored.texture)
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

fn decode(path: &Path) -> Result<egui::ColorImage, DecodeError> {
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
    let rgba = reader.decode()?.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        rgba.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_png(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zenha-avatars-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn decode_reads_png_and_rejects_garbage() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let good = temp_png("good.png", &png);
        assert_eq!(decode(&good).unwrap().size, [4, 4]);
        let bad = temp_png("bad.png", b"not an image");
        assert!(decode(&bad).is_err());
        assert!(decode(&bad.with_file_name("missing.png")).is_err());
    }

    #[test]
    fn texture_requires_matching_version() {
        let ctx = egui::Context::default();
        assert!(texture(&ctx, "ana", Some("v1")).is_none());
        let mut avatars = Avatars::default();
        avatars.register_image(&ctx, "ana", "v1", egui::ColorImage::example());
        assert!(texture(&ctx, "ana", Some("v1")).is_some());
        assert!(texture(&ctx, "ana", Some("v2")).is_none());
        assert!(texture(&ctx, "ana", None).is_none());
        assert!(texture(&ctx, "bob", Some("v1")).is_none());
    }

    #[test]
    fn evicts_oldest_textures_beyond_cap() {
        let ctx = egui::Context::default();
        let mut avatars = Avatars::default();
        let pixel = || egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]);
        for i in 0..MAX_CACHED_AVATARS {
            avatars.register_image(&ctx, &format!("user-{i}"), "v1", pixel());
        }
        // Refreshing user-0 moves it to the back so user-1 becomes the oldest.
        avatars.register_image(&ctx, "user-0", "v2", pixel());
        avatars.register_image(&ctx, "user-extra", "v1", pixel());

        assert_eq!(avatars.order.len(), MAX_CACHED_AVATARS);
        assert!(texture(&ctx, "user-1", Some("v1")).is_none());
        assert!(texture(&ctx, "user-0", Some("v2")).is_some());
        assert!(texture(&ctx, "user-2", Some("v1")).is_some());
        assert!(texture(&ctx, "user-extra", Some("v1")).is_some());
    }

    #[test]
    fn clear_unused_drops_unmatched_textures() {
        let ctx = egui::Context::default();
        let mut avatars = Avatars::default();
        let pixel = || egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]);
        avatars.register_image(&ctx, "me", "v1", pixel());
        avatars.register_image(&ctx, "ana", "v1", pixel());
        avatars.register_image(&ctx, "bob", "v1", pixel());

        avatars.clear_unused(&ctx, |id| id == "me");

        assert_eq!(avatars.order.len(), 1);
        assert!(texture(&ctx, "me", Some("v1")).is_some());
        assert!(texture(&ctx, "ana", Some("v1")).is_none());
        assert!(texture(&ctx, "bob", Some("v1")).is_none());
    }
}
