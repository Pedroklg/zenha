//! Registering and listening for global hotkeys on Windows via `zenha_core::win_platform::hotkeys`.

use std::sync::Arc;

use eframe::egui;
use zenha_core::win_platform::hotkeys::{WinHotkeys, parse_binding_spec};

use crate::platform::hotkeys::HotkeyAction;

pub struct GlobalKeys {
    manager: WinHotkeys,
    active: Vec<(u32, HotkeyAction)>,
    applied: Vec<(HotkeyAction, String)>,
    next_id: u32,
}

impl GlobalKeys {
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let ctx = ctx.clone();
        let wake = Arc::new(move || ctx.request_repaint());
        let manager = match WinHotkeys::new(wake) {
            Ok(manager) => manager,
            Err(err) => {
                tracing::warn!(%err, "could not create global hotkey manager");
                return None;
            }
        };
        Some(Self {
            manager,
            active: Vec::new(),
            applied: Vec::new(),
            next_id: 1,
        })
    }

    pub fn sync(&mut self, wanted: &[(HotkeyAction, String)]) -> Option<Vec<HotkeyAction>> {
        if self.applied == wanted {
            return None;
        }
        let mut failed = Vec::new();
        let mut specs = Vec::with_capacity(wanted.len());
        let mut active = Vec::with_capacity(wanted.len());

        for (action, binding) in wanted {
            match parse_binding_spec(binding) {
                Ok(spec) => {
                    let id = self.next_id;
                    self.next_id = self.next_id.wrapping_add(1).max(1);
                    specs.push((id, spec));
                    active.push((id, *action));
                }
                Err(err) => {
                    tracing::warn!(%binding, %err, "could not parse global hotkey");
                    failed.push(*action);
                }
            }
        }

        match self.manager.set_bindings(&specs) {
            Ok(()) => {
                self.active = active;
            }
            Err(err) => {
                tracing::warn!(%err, "could not register global hotkeys");
                for (_, action) in active {
                    failed.push(action);
                }
                self.active.clear();
            }
        }

        self.applied = wanted.to_vec();
        Some(failed)
    }

    pub fn events(&mut self) -> Vec<(HotkeyAction, bool)> {
        let mut out = Vec::new();
        for event in self.manager.drain_events() {
            if let Some(&(_, action)) = self.active.iter().find(|(id, _)| *id == event.id) {
                out.push((action, event.pressed));
            }
        }
        out
    }
}
