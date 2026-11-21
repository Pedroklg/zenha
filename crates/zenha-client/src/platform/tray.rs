//! System tray icon and menu on Windows via `zenha_core::win_platform::tray`.

#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use eframe::egui;
#[cfg(windows)]
use zenha_core::win_platform::tray::{WinTray, WinTrayEvent};

#[cfg(any(windows, test))]
const ICON_SIZE: u32 = 32;

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    ToggleMute,
    Quit,
}

#[cfg(windows)]
pub struct Tray {
    inner: WinTray,
    applied: Option<Option<bool>>,
}

#[cfg(windows)]
impl Tray {
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let rgba = match icon_rgba(ICON_SIZE) {
            Ok(rgba) => rgba,
            Err(err) => {
                tracing::warn!(%err, "could not decode tray icon image");
                return None;
            }
        };
        let muted_rgba = with_muted_badge(&rgba, ICON_SIZE);
        let ctx = ctx.clone();
        let wake = Arc::new(move || ctx.request_repaint());
        let inner = match WinTray::new(rgba, muted_rgba, ICON_SIZE, crate::APP_NAME, wake) {
            Ok(tray) => tray,
            Err(err) => {
                tracing::warn!(%err, "could not create tray icon");
                return None;
            }
        };
        Some(Self {
            inner,
            applied: Some(None),
        })
    }

    pub fn actions(&mut self) -> Vec<TrayAction> {
        let mut out = Vec::new();
        for event in self.inner.drain_events() {
            let action = match event {
                WinTrayEvent::Open => TrayAction::Open,
                WinTrayEvent::ToggleMute => {
                    self.applied = None;
                    TrayAction::ToggleMute
                }
                WinTrayEvent::Quit => TrayAction::Quit,
            };
            out.push(action);
        }
        out
    }

    pub fn sync(&mut self, muted: Option<bool>) {
        if self.applied == Some(muted) {
            return;
        }
        let tooltip = if muted == Some(true) {
            format!("{} (microfone desligado)", crate::APP_NAME)
        } else {
            crate::APP_NAME.to_owned()
        };
        self.inner.set_state(muted, &tooltip);
        self.applied = Some(muted);
    }
}

#[cfg(any(windows, test))]
fn icon_rgba(size: u32) -> Result<Vec<u8>, image::ImageError> {
    let icon = image::load_from_memory(crate::theme::APP_ICON_PNG)?;
    Ok(icon
        .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
        .into_rgba8()
        .into_raw())
}

#[cfg(any(windows, test))]
fn with_muted_badge(base: &[u8], size: u32) -> Vec<u8> {
    const RED: [f32; 3] = [235.0, 64.0, 64.0];
    let s = size as f32;
    let radius = s * 0.24;
    let gap = s * 0.06;
    let c = s - radius - 0.5;
    let half = radius * 0.55;
    let (ax, ay, bx, by) = (c - half, c - half, c + half, c + half);
    let width = (s * 0.06).max(1.0);
    let mut out = base.to_vec();
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((px - c).powi(2) + (py - c).powi(2)).sqrt();
            let cut = (radius + gap + 0.5 - d).clamp(0.0, 1.0);
            if cut <= 0.0 {
                continue;
            }
            let i = ((y * size + x) * 4) as usize;
            let fill = (radius + 0.5 - d).clamp(0.0, 1.0);
            let t = (((px - ax) * (bx - ax) + (py - ay) * (by - ay))
                / ((bx - ax).powi(2) + (by - ay).powi(2)))
            .clamp(0.0, 1.0);
            let line_d =
                ((px - ax - t * (bx - ax)).powi(2) + (py - ay - t * (by - ay)).powi(2)).sqrt();
            let slash = (width / 2.0 + 0.5 - line_d).clamp(0.0, 1.0) * fill;
            let base_a = f32::from(out[i + 3]) * (1.0 - cut);
            let alpha = fill * 255.0 + base_a * (1.0 - fill);
            for k in 0..3 {
                let badge = RED[k] * (1.0 - slash) + 255.0 * slash;
                let mixed = if alpha > 0.0 {
                    (badge * fill * 255.0 + f32::from(out[i + k]) * base_a * (1.0 - fill)) / alpha
                } else {
                    0.0
                };
                out[i + k] = mixed.round().clamp(0.0, 255.0) as u8;
            }
            out[i + 3] = alpha.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_rgba_decodes_app_icon_with_transparent_corners() {
        let rgba = icon_rgba(ICON_SIZE).expect("decode icon");
        assert_eq!(rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
        assert_eq!(rgba[3], 0);
        assert!(rgba.chunks_exact(4).any(|px| px[3] == 255));
    }

    #[test]
    fn muted_badge_paints_a_red_dot_in_the_corner_and_keeps_the_rest() {
        let base = icon_rgba(ICON_SIZE).expect("decode icon");
        let muted = with_muted_badge(&base, ICON_SIZE);
        let px = |rgba: &[u8], x: u32, y: u32| {
            let i = ((y * ICON_SIZE + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
        };
        let edge = px(&muted, 29, 24);
        assert_eq!(edge[3], 255);
        assert!(edge[0] > 200 && edge[1] < 100 && edge[2] < 100, "{edge:?}");
        let middle = px(&muted, 24, 24);
        assert!(
            middle[0] > 200 && middle[1] > 200 && middle[2] > 200,
            "{middle:?}"
        );
        assert_eq!(px(&muted, 8, 8), px(&base, 8, 8));
        assert_eq!(px(&muted, 0, 0), px(&base, 0, 0));
    }
}
