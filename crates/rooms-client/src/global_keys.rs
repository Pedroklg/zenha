//! Registro e escuta de atalhos globais no Windows via `global-hotkey`.

use std::sync::mpsc::{Receiver, channel};

use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::hotkeys::HotkeyAction;

pub struct GlobalKeys {
    manager: GlobalHotKeyManager,
    active: Vec<(HotkeyAction, HotKey)>,
    applied: Vec<(HotkeyAction, String)>,
    events: Receiver<u32>,
}

impl GlobalKeys {
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => manager,
            Err(err) => {
                tracing::warn!(%err, "could not create global hotkey manager");
                return None;
            }
        };
        let (tx, events) = channel();
        let ctx = ctx.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state == HotKeyState::Pressed && tx.send(event.id).is_ok() {
                ctx.request_repaint();
            }
        }));
        Some(Self {
            manager,
            active: Vec::new(),
            applied: Vec::new(),
            events,
        })
    }

    pub fn sync(&mut self, wanted: &[(HotkeyAction, String)]) -> Option<Vec<HotkeyAction>> {
        if self.applied == wanted {
            return None;
        }
        for (_, hotkey) in self.active.drain(..) {
            if let Err(err) = self.manager.unregister(hotkey) {
                tracing::warn!(%err, "could not unregister global hotkey");
            }
        }
        let mut failed = Vec::new();
        for (action, binding) in wanted {
            let hotkey = match binding.parse::<HotKey>() {
                Ok(hotkey) => hotkey,
                Err(err) => {
                    tracing::warn!(%binding, %err, "could not parse global hotkey");
                    failed.push(*action);
                    continue;
                }
            };
            match self.manager.register(hotkey) {
                Ok(()) => self.active.push((*action, hotkey)),
                Err(err) => {
                    tracing::warn!(%binding, %err, "could not register global hotkey");
                    failed.push(*action);
                }
            }
        }
        self.applied = wanted.to_vec();
        Some(failed)
    }

    pub fn pressed(&mut self) -> Vec<HotkeyAction> {
        let mut actions = Vec::new();
        while let Ok(id) = self.events.try_recv() {
            if let Some(&(action, _)) = self.active.iter().find(|(_, hotkey)| hotkey.id() == id) {
                actions.push(action);
            }
        }
        actions
    }
}
