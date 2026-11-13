//! Ícone e menu da bandeja do sistema no Windows via `tray-icon`.

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
#[cfg(any(windows, test))]
const AMBER: [u8; 3] = [0xF5, 0xA5, 0x24];

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    ToggleMute,
    Quit,
}

#[cfg(windows)]
pub struct Tray {
    _icon: TrayIcon,
    mute: CheckMenuItem,
    events: Receiver<TrayAction>,
    applied: Option<Option<bool>>,
}

#[cfg(windows)]
impl Tray {
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let icon = match Icon::from_rgba(icon_rgba(ICON_SIZE), ICON_SIZE, ICON_SIZE) {
            Ok(icon) => icon,
            Err(err) => {
                tracing::warn!(%err, "could not create tray icon image");
                return None;
            }
        };
        let open = MenuItem::new("Abrir", true, None);
        let mute = CheckMenuItem::new("Microfone mutado", false, false, None);
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
            _icon: icon,
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
        self.applied = Some(muted);
    }
}

#[cfg(any(windows, test))]
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
    use super::*;

    #[test]
    fn icon_rgba_draws_amber_circle_on_transparent_background() {
        let rgba = icon_rgba(ICON_SIZE);
        assert_eq!(rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);

        let mid = ((ICON_SIZE / 2) * ICON_SIZE + (ICON_SIZE / 2)) as usize * 4;
        assert_eq!(&rgba[mid..mid + 4], &[0xF5, 0xA5, 0x24, 0xFF]);

        assert_eq!(&rgba[0..4], &[0, 0, 0, 0]);
        let last = rgba.len() - 4;
        assert_eq!(&rgba[last..last + 4], &[0, 0, 0, 0]);

        assert!(rgba.chunks_exact(4).any(|px| px[3] > 0 && px[3] < 255));
    }
}
