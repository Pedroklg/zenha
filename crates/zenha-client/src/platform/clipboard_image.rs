//! Preparing images pasted from the clipboard for sending in chat.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::ColorImage;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use zenha_protocol::MAX_FILE_BYTES;

use crate::format::DISPLAY_UTC_OFFSET_MINUTES;

const PASTE_DIR_NAME: &str = "zenha-colados";
const FILE_PREFIX: &str = "print-";
const FILE_EXT: &str = ".png";
const MAX_COLLISION_ATTEMPTS: u32 = 99;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteImageError {
    Empty,
    TooLarge,
    PrepareFailed,
}

impl PasteImageError {
    pub fn user_message(self) -> &'static str {
        match self {
            Self::Empty => "Nenhuma imagem na área de transferência.",
            Self::TooLarge => "A imagem colada passa do limite de 50 MB.",
            Self::PrepareFailed => "Não foi possível preparar a imagem colada.",
        }
    }
}

impl fmt::Display for PasteImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.user_message())
    }
}

impl std::error::Error for PasteImageError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPastedImage {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

/// Temporary folder used to save pasted images before sending.
pub fn paste_temp_dir() -> PathBuf {
    std::env::temp_dir().join(PASTE_DIR_NAME)
}

/// Encodes the pasted image as PNG and writes a temporary file in `dir`.
pub fn save_pasted_image(
    image: &ColorImage,
    dir: &Path,
) -> Result<SavedPastedImage, PasteImageError> {
    let [width, height] = image.size;
    let rgba = color_image_to_rgba(image)?;
    let png = encode_rgba_to_png(width, height, &rgba, MAX_FILE_BYTES)?;
    let size = u64::try_from(png.len()).map_err(|_| PasteImageError::PrepareFailed)?;
    let now_secs = current_unix_secs()?;
    let base_stem = format_paste_stem_at(now_secs, DISPLAY_UTC_OFFSET_MINUTES);
    let (path, name) = write_unique_png(dir, &base_stem, &png)?;
    Ok(SavedPastedImage { path, name, size })
}

pub fn delete_in_background(path: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("delete-pasted-image".to_owned())
        .spawn(move || delete_pasted_file(&path));
    if let Err(err) = spawned {
        tracing::debug!(%err, "could not start pasted image delete thread");
    }
}

fn delete_pasted_file(path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(%err, path = %path.display(), "could not remove pasted image");
    }
}

/// Removes old `print-*.png` temporary files left by previous sessions.
pub fn cleanup_old_pastes(dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::debug!(%err, path = %dir.display(), "could not list pasted images directory");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_paste_png = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(FILE_PREFIX) && name.ends_with(FILE_EXT));
        if is_paste_png && let Err(err) = fs::remove_file(&path) {
            tracing::debug!(%err, path = %path.display(), "could not remove stale pasted image");
        }
    }
}

#[cfg(test)]
fn format_paste_filename_at(unix_secs: i64, utc_offset_minutes: i32) -> String {
    format!(
        "{}{FILE_EXT}",
        format_paste_stem_at(unix_secs, utc_offset_minutes)
    )
}

fn format_paste_stem_at(unix_secs: i64, utc_offset_minutes: i32) -> String {
    let offset_secs = i64::from(utc_offset_minutes).saturating_mul(60);
    let local_secs = unix_secs.saturating_add(offset_secs);
    let days = local_secs.div_euclid(86_400);
    let secs_of_day = local_secs.rem_euclid(86_400);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{FILE_PREFIX}{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

// Howard Hinnant's civil-from-days algorithm relative to 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

fn color_image_to_rgba(image: &ColorImage) -> Result<Vec<u8>, PasteImageError> {
    let [width, height] = image.size;
    if width == 0 || height == 0 || image.pixels.is_empty() {
        return Err(PasteImageError::Empty);
    }
    let Some(expected_pixels) = width.checked_mul(height) else {
        return Err(PasteImageError::PrepareFailed);
    };
    if image.pixels.len() != expected_pixels {
        return Err(PasteImageError::PrepareFailed);
    }
    let Some(byte_len) = expected_pixels.checked_mul(4) else {
        return Err(PasteImageError::PrepareFailed);
    };
    let mut rgba = Vec::with_capacity(byte_len);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }
    Ok(rgba)
}

pub fn encode_rgba_to_png(
    width: usize,
    height: usize,
    rgba: &[u8],
    max_bytes: u64,
) -> Result<Vec<u8>, PasteImageError> {
    if width == 0 || height == 0 || rgba.is_empty() {
        return Err(PasteImageError::Empty);
    }
    let Ok(width_u32) = u32::try_from(width) else {
        return Err(PasteImageError::PrepareFailed);
    };
    let Ok(height_u32) = u32::try_from(height) else {
        return Err(PasteImageError::PrepareFailed);
    };
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4));
    if expected_len != Some(rgba.len()) {
        return Err(PasteImageError::PrepareFailed);
    }

    let mut png = Vec::new();
    let encoder = PngEncoder::new(&mut png);
    encoder
        .write_image(rgba, width_u32, height_u32, ExtendedColorType::Rgba8)
        .map_err(|err| {
            tracing::warn!(%err, width, height, "could not encode pasted clipboard image as png");
            PasteImageError::PrepareFailed
        })?;

    if (png.len() as u64) > max_bytes {
        return Err(PasteImageError::TooLarge);
    }
    Ok(png)
}

fn write_unique_png(
    dir: &Path,
    base_stem: &str,
    png: &[u8],
) -> Result<(PathBuf, String), PasteImageError> {
    fs::create_dir_all(dir).map_err(|err| {
        tracing::warn!(%err, path = %dir.display(), "could not create pasted images directory");
        PasteImageError::PrepareFailed
    })?;

    for attempt in 1..=MAX_COLLISION_ATTEMPTS {
        let filename = if attempt == 1 {
            format!("{base_stem}{FILE_EXT}")
        } else {
            format!("{base_stem}-{attempt}{FILE_EXT}")
        };
        let path = dir.join(&filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(err) = file.write_all(png).and_then(|()| file.flush()) {
                    tracing::warn!(%err, path = %path.display(), "could not write pasted png file");
                    let _ = fs::remove_file(&path);
                    return Err(PasteImageError::PrepareFailed);
                }
                return Ok((path, filename));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "could not create pasted png file");
                return Err(PasteImageError::PrepareFailed);
            }
        }
    }

    Err(PasteImageError::PrepareFailed)
}

fn current_unix_secs() -> Result<i64, PasteImageError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| {
            tracing::warn!(%err, "system clock precedes unix epoch");
            PasteImageError::PrepareFailed
        })?;
    i64::try_from(elapsed.as_secs()).map_err(|_| PasteImageError::PrepareFailed)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use eframe::egui::Color32;

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn isolated_test_dir(label: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "zenha-clipboard-test-{label}-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn filename_formats_local_date_and_time() {
        // 2026-11-15 16:45:09 UTC -> 13:45:09 in UTC-3
        assert_eq!(
            format_paste_filename_at(1_794_761_109, -180),
            "print-20261115-134509.png"
        );
        // Year rollover across UTC-3 boundary: 2025-01-01 02:59:59 UTC -> 2024-12-31 23:59:59
        assert_eq!(
            format_paste_filename_at(1_735_700_399, -180),
            "print-20241231-235959.png"
        );
        // Leap day: 2024-02-29 12:00:00 UTC -> 2024-02-29 09:00:00
        assert_eq!(
            format_paste_filename_at(1_709_208_000, -180),
            "print-20240229-090000.png"
        );
    }

    #[test]
    fn encodes_rgba_to_valid_png_and_decodes_back() -> Result<(), Box<dyn std::error::Error>> {
        let rgba = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];
        let png = encode_rgba_to_png(2, 2, &rgba, MAX_FILE_BYTES)?;
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let decoded = image::load_from_memory(&png)?;
        let decoded_rgba = decoded.to_rgba8();
        assert_eq!(decoded_rgba.dimensions(), (2, 2));
        assert_eq!(decoded_rgba.as_raw(), &rgba);
        Ok(())
    }

    #[test]
    fn rejects_empty_mismatched_and_oversized_images() {
        assert_eq!(
            encode_rgba_to_png(0, 10, &[], MAX_FILE_BYTES),
            Err(PasteImageError::Empty)
        );
        assert_eq!(
            encode_rgba_to_png(2, 2, &[0; 12], MAX_FILE_BYTES),
            Err(PasteImageError::PrepareFailed)
        );
        let rgba = vec![120; 4 * 4 * 4];
        assert_eq!(
            encode_rgba_to_png(4, 4, &rgba, 10),
            Err(PasteImageError::TooLarge)
        );
    }

    #[test]
    fn save_pasted_image_handles_same_second_collisions_and_cleanup()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = isolated_test_dir("save-cleanup");
        let image = ColorImage::new(
            [2, 1],
            vec![
                Color32::from_rgb(20, 40, 60),
                Color32::from_rgb(80, 100, 120),
            ],
        );

        let first = save_pasted_image(&image, &dir)?;
        let second = save_pasted_image(&image, &dir)?;
        assert_ne!(first.path, second.path);
        assert!(first.path.exists());
        assert!(second.path.exists());
        assert_eq!(first.size, fs::metadata(&first.path)?.len());
        assert!(first.name.starts_with("print-") && first.name.ends_with(".png"));
        assert_eq!(
            first.path.file_name().and_then(|n| n.to_str()),
            Some(first.name.as_str())
        );

        let keep = dir.join("unrelated.txt");
        fs::write(&keep, b"keep")?;

        delete_pasted_file(&first.path);
        assert!(!first.path.exists());
        delete_pasted_file(&first.path);
        assert!(second.path.exists());

        cleanup_old_pastes(&dir);
        assert!(!second.path.exists());
        assert!(keep.exists());

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }
}
