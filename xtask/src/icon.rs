//! Geração do ícone do aplicativo (`zenha.ico`) a partir do desenho da bandeja.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use image::ExtendedColorType;
use image::codecs::ico::{IcoEncoder, IcoFrame};

const ICON_SIZES: &[u32] = &[16, 32, 48, 256];
const AMBER: [u8; 3] = [0xF5, 0xA5, 0x24];

pub fn default_output_path() -> io::Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .ok_or_else(|| io::Error::other("xtask manifest dir has no parent"))?;
    Ok(root
        .join("crates")
        .join("zenha-client")
        .join("assets")
        .join("zenha.ico"))
}

pub fn generate(output: &Path) -> Result<(), Box<dyn Error>> {
    let bytes = encode_ico(ICON_SIZES)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, &bytes)?;
    eprintln!("wrote {} ({} bytes)", output.display(), bytes.len());
    Ok(())
}

pub fn encode_ico(sizes: &[u32]) -> Result<Vec<u8>, image::ImageError> {
    let mut frames = Vec::with_capacity(sizes.len());
    for &size in sizes {
        let rgba = icon_rgba(size);
        frames.push(IcoFrame::as_png(
            &rgba,
            size,
            size,
            ExtendedColorType::Rgba8,
        )?);
    }
    let mut out = Vec::new();
    IcoEncoder::new(&mut out).encode_images(&frames)?;
    Ok(out)
}

// Keep in sync with `crates/zenha-client/src/platform/tray.rs`.
fn icon_rgba(size: u32) -> Vec<u8> {
    let center = size as f32 / 2.0;
    let radius = (center - 1.0).max(0.0);
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let dist = dx.hypot(dy);
            let alpha = (radius + 0.5 - dist).clamp(0.0, 1.0);
            let a = (alpha * 255.0).round() as u8;
            if a == 0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                rgba.extend_from_slice(&[AMBER[0], AMBER[1], AMBER[2], a]);
            }
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::codecs::ico::IcoDecoder;
    use image::{ColorType, ImageDecoder};

    use super::*;

    #[test]
    fn icon_rgba_draws_amber_circle_on_transparent_background() {
        let size = 32;
        let rgba = icon_rgba(size);
        assert_eq!(rgba.len(), (size * size * 4) as usize);

        let mid = ((size / 2) * size + (size / 2)) as usize * 4;
        assert_eq!(&rgba[mid..mid + 4], &[0xF5, 0xA5, 0x24, 0xFF]);

        assert_eq!(&rgba[0..4], &[0, 0, 0, 0]);
        let last = rgba.len() - 4;
        assert_eq!(&rgba[last..last + 4], &[0, 0, 0, 0]);

        assert!(rgba.chunks_exact(4).any(|px| px[3] > 0 && px[3] < 255));
    }

    #[test]
    fn encode_ico_writes_all_requested_sizes() {
        let bytes = encode_ico(ICON_SIZES).unwrap();
        assert_eq!(&bytes[0..6], &[0, 0, 1, 0, 4, 0]);

        let expected_dims: [(u8, u8); 4] = [(16, 16), (32, 32), (48, 48), (0, 0)];
        for (i, &(w, h)) in expected_dims.iter().enumerate() {
            let entry = &bytes[6 + i * 16..6 + (i + 1) * 16];
            assert_eq!((entry[0], entry[1]), (w, h));
            assert_eq!(u16::from_le_bytes([entry[6], entry[7]]), 32);
        }

        let decoder = IcoDecoder::new(Cursor::new(&bytes)).unwrap();
        assert_eq!(decoder.dimensions(), (256, 256));
        assert_eq!(decoder.color_type(), ColorType::Rgba8);
        let mut decoded = vec![0u8; decoder.total_bytes() as usize];
        decoder.read_image(&mut decoded).unwrap();
        assert_eq!(decoded.len(), 256 * 256 * 4);
    }
}
