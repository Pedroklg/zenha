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
        let rgba = match icon_rgba(ICON_SIZE) {
            Ok(rgba) => rgba,
            Err(err) => {
                tracing::warn!(%err, "could not decode tray icon image");
                return None;
            }
        };
        let icon = match Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE) {
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
fn icon_rgba(size: u32) -> Result<Vec<u8>, image::ImageError> {
    let icon = image::load_from_memory(crate::theme::APP_ICON_PNG)?;
    Ok(icon
        .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
        .into_rgba8()
        .into_raw())
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
}
