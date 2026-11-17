//! Compartilhamento de tela e dispositivos de áudio simulados.

use std::time::Duration;

use crate::core_session::TestFeed;
use crate::ui_model::UiEvent;

use super::DemoSession;
use super::fixtures::{demo_devices, demo_targets};

impl DemoSession {
    pub(super) fn reset_share(&mut self) {
        self.share_preview = None;
        self.ana_sharing = false;
        self.watch = None;
    }

    pub(super) fn audio_devices_event(&self) -> UiEvent {
        UiEvent::AudioDevicesUpdated {
            inputs: demo_devices("in", &["Microfone (headset USB)", "Microfone embutido"]),
            outputs: demo_devices(
                "out",
                &["Alto-falantes (Realtek)", "Fones de ouvido (headset USB)"],
            ),
            selected_input: self.input.clone(),
            selected_output: self.output.clone(),
        }
    }

    pub(super) fn cancel_capture_targets(&mut self) {
        self.scheduled.retain(|s| {
            !matches!(
                s.event,
                UiEvent::CaptureTargetsUpdated(_) | UiEvent::CaptureThumbnail { .. }
            )
        });
    }

    pub(super) fn publish_audio_devices(&mut self) {
        let event = self.audio_devices_event();
        self.schedule(Duration::ZERO, event);
    }

    pub(super) fn start_share(&mut self, target_id: &str) {
        if self.room.is_none() || self.share_preview.is_some() {
            return;
        }
        self.cancel_capture_targets();
        if demo_targets().iter().any(|t| t.id == target_id) {
            let Some(feed) = TestFeed::start_with(480, 270, 10) else {
                return;
            };
            let preview = feed.handle();
            self.share_preview = Some(feed);
            self.schedule(Duration::ZERO, UiEvent::ScreenShareStarted { preview });
        } else {
            self.schedule(
                Duration::ZERO,
                UiEvent::ShareFailed {
                    message: "A janela ou tela escolhida não está mais disponível.".to_owned(),
                },
            );
        }
    }

    pub(super) fn watch_share(&mut self, identity: String) {
        if !(self.room.is_some() && self.ana_sharing && identity == "demo-ana") {
            return;
        }
        self.watch = None;
        let Some(feed) = TestFeed::start() else {
            return;
        };
        let handle = feed.handle();
        self.watch = Some(feed);
        self.schedule(
            Duration::ZERO,
            UiEvent::WatchingStarted {
                identity,
                feed: handle,
            },
        );
    }
}
