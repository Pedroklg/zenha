//! Processing and normalization of profile photos on the server.

use std::fmt::Write as _;
use std::io::Cursor;

use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder as _, ImageEncoder as _, ImageFormat,
    ImageReader, Limits,
};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use zenha_protocol::{AVATAR_SIZE_PX, MAX_AVATAR_UPLOAD_BYTES};

const MAX_AVATAR_DECODE_SIDE: u32 = 8_192;
const MAX_AVATAR_DECODE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AvatarProcessError {
    #[error("image body is empty")]
    Empty,
    #[error("image exceeds {} bytes limit", MAX_AVATAR_UPLOAD_BYTES)]
    TooLarge,
    #[error("unsupported image format; expected PNG, JPEG, or WebP")]
    UnsupportedFormat,
    #[error("image has invalid dimensions")]
    InvalidDimensions,
    #[error("failed to decode image: {0}")]
    Decode(String),
    #[error("failed to encode PNG: {0}")]
    Encode(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedAvatar {
    pub png: Vec<u8>,
    pub version: String,
}

/// Decodes bytes sent by the client, center-crops and re-encodes as a 128x128 PNG.
pub fn process_avatar(raw: &[u8]) -> Result<ProcessedAvatar, AvatarProcessError> {
    if raw.is_empty() {
        return Err(AvatarProcessError::Empty);
    }
    if raw.len() > MAX_AVATAR_UPLOAD_BYTES {
        return Err(AvatarProcessError::TooLarge);
    }

    let mut reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .map_err(|err| AvatarProcessError::Decode(err.to_string()))?;

    match reader.format() {
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP) => {}
        _ => return Err(AvatarProcessError::UnsupportedFormat),
    }

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_AVATAR_DECODE_SIDE);
    limits.max_image_height = Some(MAX_AVATAR_DECODE_SIDE);
    limits.max_alloc = Some(MAX_AVATAR_DECODE_ALLOC_BYTES);
    reader.limits(limits.clone());

    let mut decoder = reader
        .into_decoder()
        .map_err(|err| AvatarProcessError::Decode(err.to_string()))?;
    limits
        .reserve(decoder.total_bytes())
        .map_err(|err| AvatarProcessError::Decode(err.to_string()))?;
    decoder
        .set_limits(limits)
        .map_err(|err| AvatarProcessError::Decode(err.to_string()))?;

    let orientation = decoder
        .orientation()
        .map_err(|err| AvatarProcessError::Decode(err.to_string()))?;
    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|err| AvatarProcessError::Decode(err.to_string()))?;
    image.apply_orientation(orientation);

    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err(AvatarProcessError::InvalidDimensions);
    }

    let side = width.min(height);
    let offset_x = (width - side) / 2;
    let offset_y = (height - side) / 2;
    let cropped = image.crop_imm(offset_x, offset_y, side, side);
    let normalized = if side == AVATAR_SIZE_PX {
        cropped
    } else {
        cropped.resize_exact(AVATAR_SIZE_PX, AVATAR_SIZE_PX, FilterType::Lanczos3)
    };

    let rgba = normalized.to_rgba8();
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            rgba.as_raw(),
            AVATAR_SIZE_PX,
            AVATAR_SIZE_PX,
            ExtendedColorType::Rgba8,
        )
        .map_err(|err| AvatarProcessError::Encode(err.to_string()))?;

    let version = short_sha256_hex(&png);
    Ok(ProcessedAvatar { png, version })
}

fn short_sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{ImageFormat, ImageReader, Rgb, RgbImage, Rgba, RgbaImage};
    use zenha_protocol::{AVATAR_SIZE_PX, MAX_AVATAR_UPLOAD_BYTES, validate_avatar_version};

    use super::{AvatarProcessError, process_avatar};

    fn encode_rgba_png(img: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    fn encode_rgb(img: &RgbImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img.clone())
            .write_to(&mut Cursor::new(&mut out), format)
            .unwrap();
        out
    }

    #[test]
    fn rejects_empty_oversized_corrupt_and_unsupported_formats() {
        assert_eq!(process_avatar(&[]), Err(AvatarProcessError::Empty));

        let too_big = vec![0u8; MAX_AVATAR_UPLOAD_BYTES + 1];
        assert_eq!(process_avatar(&too_big), Err(AvatarProcessError::TooLarge));

        assert!(matches!(
            process_avatar(b"not an image"),
            Err(AvatarProcessError::UnsupportedFormat)
        ));

        let bmp = encode_rgb(&RgbImage::new(8, 8), ImageFormat::Bmp);
        assert_eq!(
            process_avatar(&bmp),
            Err(AvatarProcessError::UnsupportedFormat)
        );

        let mut corrupt_png = encode_rgba_png(&RgbaImage::new(16, 16));
        corrupt_png.truncate(12);
        assert!(matches!(
            process_avatar(&corrupt_png),
            Err(AvatarProcessError::Decode(_))
        ));
    }

    #[test]
    fn crops_center_square_and_outputs_128x128_png_for_supported_formats() {
        let mut rect = RgbaImage::from_fn(200, 100, |x, _| {
            if !(50..150).contains(&x) {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 200, 50, 255])
            }
        });
        let png_input = encode_rgba_png(&rect);
        let processed = process_avatar(&png_input).unwrap();
        assert_eq!(validate_avatar_version(&processed.version), Ok(()));
        assert_eq!(processed.version.len(), 16);

        let decoded = ImageReader::new(Cursor::new(&processed.png))
            .with_guessed_format()
            .unwrap();
        assert_eq!(decoded.format(), Some(ImageFormat::Png));
        let out_img = decoded.decode().unwrap().to_rgba8();
        assert_eq!(out_img.dimensions(), (AVATAR_SIZE_PX, AVATAR_SIZE_PX));
        // Center crop removes the outer red bands (0..50 and 150..200).
        let center_px = out_img.get_pixel(64, 64);
        assert_eq!(*center_px, Rgba([0, 200, 50, 255]));

        // Changing image bytes changes the content version hash.
        rect.put_pixel(100, 50, Rgba([255, 255, 255, 255]));
        let changed = process_avatar(&encode_rgba_png(&rect)).unwrap();
        assert_ne!(changed.version, processed.version);

        // Deterministic for identical pixels.
        let repeat = process_avatar(&png_input).unwrap();
        assert_eq!(repeat.version, processed.version);

        // JPEG & WebP are normalized to 128x128 PNG too.
        let rgb = RgbImage::from_pixel(64, 96, Rgb([20, 40, 180]));
        let jpeg_out = process_avatar(&encode_rgb(&rgb, ImageFormat::Jpeg)).unwrap();
        let webp_out = process_avatar(&encode_rgb(&rgb, ImageFormat::WebP)).unwrap();
        for item in [jpeg_out, webp_out] {
            let reader = ImageReader::new(Cursor::new(&item.png))
                .with_guessed_format()
                .unwrap();
            assert_eq!(reader.format(), Some(ImageFormat::Png));
            let decoded = reader.decode().unwrap();
            assert_eq!(
                (decoded.width(), decoded.height()),
                (AVATAR_SIZE_PX, AVATAR_SIZE_PX)
            );
        }
    }
}
