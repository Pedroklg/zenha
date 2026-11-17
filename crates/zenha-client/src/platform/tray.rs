//! System tray icon and menu on Windows via `tray-icon`.

#[cfg(windows)]
use std::sync::mpsc::{Receiver, channel};

#[cfg(windows)]
use eframe::egui;
#[cfg(windows)]
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
#[cfg(windows)]
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

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
    icon: TrayIcon,
    normal_rgba: Vec<u8>,
    muted_rgba: Vec<u8>,
    mute: CheckMenuItem,
    events: Receiver<TrayAction>,
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
        let icon = match Icon::from_rgba(rgba.clone(), ICON_SIZE, ICON_SIZE) {
            Ok(icon) => icon,
            Err(err) => {
                tracing::warn!(%err, "could not create tray icon image");
                return None;
            }
        };
        let open = MenuItem::new("Abrir", true, None);
        let mute = CheckMenuItem::new("Microfone desligado", false, false, None);
        let quit = MenuItem::new("Sair", true, None);
        let menu = Menu::new();
        if let Err(err) =
            menu.append_items(&[&open, &mute, &PredefinedMenuItem::separator(), &quit])
        {
            tracing::warn!(%err, "could not build tray menu");
            return None;
        }
        let icon = match TrayIconBuilder::new()
            .with_tooltip(crate::APP_NAME)
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
        {
            Ok(icon) => icon,
            Err(err) => {
                tracing::warn!(%err, "could not create tray icon");
                return None;
            }
        };
        let open_id: MenuId = open.into_id();
        let mute_id: MenuId = mute.id().clone();
        let quit_id: MenuId = quit.into_id();
        let (tx, events) = channel();
        let tray_tx = tx.clone();
        let tray_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let is_open = matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            );
            if is_open && tray_tx.send(TrayAction::Open).is_ok() {
                tray_ctx.request_repaint();
            }
        }));
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == open_id {
                Some(TrayAction::Open)
            } else if event.id == mute_id {
                Some(TrayAction::ToggleMute)
            } else if event.id == quit_id {
                Some(TrayAction::Quit)
            } else {
                None
            };
            if let Some(action) = action
                && tx.send(action).is_ok()
            {
                menu_ctx.request_repaint();
            }
        }));
        Some(Self {
            icon,
            normal_rgba: rgba,
            muted_rgba,
            mute,
            events,
            applied: Some(None),
        })
    }

    pub fn actions(&mut self) -> Vec<TrayAction> {
        let mut out = Vec::new();
        while let Ok(action) = self.events.try_recv() {
            if action == TrayAction::ToggleMute {
                self.applied = None;
            }
            out.push(action);
        }
        out
    }

    pub fn sync(&mut self, muted: Option<bool>) {
        if self.applied == Some(muted) {
            return;
        }
        self.mute.set_enabled(muted.is_some());
        self.mute.set_checked(muted.unwrap_or(false));
        self.set_muted_look(muted == Some(true));
        self.applied = Some(muted);
    }

    fn set_muted_look(&self, muted: bool) {
        let (rgba, tooltip) = if muted {
            (
                &self.muted_rgba,
                format!("{} (microfone desligado)", crate::APP_NAME),
            )
        } else {
            (&self.normal_rgba, crate::APP_NAME.to_owned())
        };
        let result = Icon::from_rgba(rgba.clone(), ICON_SIZE, ICON_SIZE)
            .map_err(|err| err.to_string())
            .and_then(|icon| {
                self.icon
                    .set_icon(Some(icon))
                    .map_err(|err| err.to_string())
            });
        if let Err(err) = result {
            tracing::warn!(%err, "could not update tray icon");
        }
        if let Err(err) = self.icon.set_tooltip(Some(tooltip)) {
            tracing::warn!(%err, "could not update tray tooltip");
        }
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
