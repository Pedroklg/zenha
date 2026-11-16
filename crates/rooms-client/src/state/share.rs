//! Estado de transmissão e visualização de tela.

use std::collections::BTreeMap;

use super::{AppState, Screen};
use crate::ui_model::{
    CaptureTargetView, FeedHandle, SharePresetView, TargetKindView, UiCommand, UiEvent,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailView {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharePicker {
    pub open: bool,
    pub targets: Vec<CaptureTargetView>,
    pub thumbnails: BTreeMap<String, ThumbnailView>,
    pub targets_serial: u64,
    pub selected: Option<String>,
    pub preset: SharePresetView,
    pub loaded: bool,
}

impl SharePicker {
    pub fn screens(&self) -> impl Iterator<Item = &CaptureTargetView> {
        self.targets
            .iter()
            .filter(|t| t.kind == TargetKindView::Screen)
    }

    pub fn windows(&self) -> impl Iterator<Item = &CaptureTargetView> {
        self.targets
            .iter()
            .filter(|t| t.kind == TargetKindView::Window)
    }

    pub fn can_start(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|id| self.targets.iter().any(|t| t.id == *id))
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.thumbnails.clear();
    }

    pub(super) fn reset_loading(&mut self) {
        self.loaded = false;
        self.thumbnails.clear();
        self.targets_serial = self.targets_serial.wrapping_add(1);
    }

    pub(super) fn set_targets(&mut self, targets: Vec<CaptureTargetView>) {
        self.targets = targets;
        self.thumbnails.clear();
        self.targets_serial = self.targets_serial.wrapping_add(1);
        self.loaded = true;
        if !self.can_start() {
            let first = self
                .screens()
                .chain(self.windows())
                .next()
                .map(|t| t.id.clone());
            self.selected = first;
        }
    }

    pub(super) fn set_thumbnail(&mut self, id: String, width: usize, height: usize, rgba: Vec<u8>) {
        if !self.open
            || width == 0
            || height == 0
            || width.saturating_mul(height).saturating_mul(4) != rgba.len()
            || !self.targets.iter().any(|t| t.id == id)
        {
            return;
        }
        self.thumbnails.insert(
            id,
            ThumbnailView {
                width,
                height,
                rgba,
            },
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalShare {
    #[default]
    Idle,
    Starting,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewState {
    pub feed: FeedHandle,
    pub serial: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchState {
    pub identity: String,
    pub feed: FeedHandle,
    pub fullscreen: bool,
    pub serial: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShareState {
    pub local: LocalShare,
    pub preview: Option<PreviewState>,
    pub error: Option<String>,
    pub remote: Vec<String>,
    pub watching: Option<WatchState>,
    serial: u64,
}

impl ShareState {
    pub fn is_sharing_remote(&self, identity: &str) -> bool {
        self.remote.iter().any(|id| id == identity)
    }

    pub fn is_watching(&self, identity: &str) -> bool {
        self.watching
            .as_ref()
            .is_some_and(|w| w.identity == identity)
    }

    fn remove_remote(&mut self, identity: &str) {
        self.remote.retain(|id| id != identity);
        if self.is_watching(identity) {
            self.watching = None;
        }
    }

    pub(super) fn apply(&mut self, event: UiEvent) {
        match event {
            UiEvent::ScreenShareStarted { preview } => {
                self.serial += 1;
                self.local = LocalShare::Active;
                self.preview = Some(PreviewState {
                    feed: preview,
                    serial: self.serial,
                });
            }
            UiEvent::ScreenShareStopped => {
                self.local = LocalShare::Idle;
                self.preview = None;
            }
            UiEvent::ShareFailed { message } => {
                self.error = Some(message);
                if self.local == LocalShare::Starting {
                    self.local = LocalShare::Idle;
                    self.preview = None;
                }
            }
            UiEvent::RemoteScreenShareStarted { identity } => {
                if !self.is_sharing_remote(&identity) {
                    self.remote.push(identity);
                }
            }
            UiEvent::RemoteScreenShareStopped { identity } => self.remove_remote(&identity),
            UiEvent::WatchingStarted { identity, feed } => {
                self.serial += 1;
                self.watching = Some(WatchState {
                    identity,
                    feed,
                    fullscreen: false,
                    serial: self.serial,
                });
            }
            UiEvent::WatchingStopped => self.watching = None,
            UiEvent::ParticipantLeft { identity } => self.remove_remote(&identity),
            _ => {}
        }
    }

    pub(super) fn start(&mut self) {
        self.error = None;
        self.local = LocalShare::Starting;
    }

    pub(super) fn watch(&mut self, identity: String) -> Vec<UiCommand> {
        if !self.is_sharing_remote(&identity) || self.is_watching(&identity) {
            return Vec::new();
        }
        vec![UiCommand::WatchScreenShare { identity }]
    }

    pub(super) fn stop_watching(&mut self) -> Vec<UiCommand> {
        if self.watching.take().is_some() {
            vec![UiCommand::StopWatching]
        } else {
            Vec::new()
        }
    }

    pub(super) fn toggle_fullscreen(&mut self) {
        if let Some(watching) = &mut self.watching {
            watching.fullscreen = !watching.fullscreen;
        }
    }
}

/// Size that fits `frame` inside `avail` keeping the aspect ratio.
pub fn aspect_fit(avail: (f32, f32), frame: (usize, usize)) -> (f32, f32) {
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    if fw <= 0.0 || fh <= 0.0 || avail.0 <= 0.0 || avail.1 <= 0.0 {
        return (0.0, 0.0);
    }
    let scale = (avail.0 / fw).min(avail.1 / fh);
    (fw * scale, fh * scale)
}

impl AppState {
    pub(super) fn open_picker(&mut self) -> Vec<UiCommand> {
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        if room.share.local != LocalShare::Idle {
            return Vec::new();
        }
        room.share.error = None;
        self.picker.open = true;
        self.picker.reset_loading();
        vec![UiCommand::RefreshCaptureTargets]
    }

    pub(super) fn start_share(&mut self) -> Vec<UiCommand> {
        if !self.picker.can_start() {
            return Vec::new();
        }
        let Some(target_id) = self.picker.selected.clone() else {
            return Vec::new();
        };
        let preset = self.picker.preset;
        let Screen::InRoom(room) = &mut self.screen else {
            return Vec::new();
        };
        if room.share.local != LocalShare::Idle {
            return Vec::new();
        }
        room.share.start();
        self.picker.close();
        vec![UiCommand::StartScreenShare { target_id, preset }]
    }

    pub fn is_watching(&self) -> bool {
        match &self.screen {
            Screen::InRoom(room) => room.share.watching.is_some(),
            _ => false,
        }
    }

    pub fn is_sharing_local(&self) -> bool {
        match &self.screen {
            Screen::InRoom(room) => room.share.local == LocalShare::Active,
            _ => false,
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        match &self.screen {
            Screen::InRoom(room) => room.share.watching.as_ref().is_some_and(|w| w.fullscreen),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{in_room, person, room, state_with_code};

    struct NoFrames;

    impl crate::ui_model::FrameFeed for NoFrames {
        fn take_newer(&self, _last_seen: &mut u64) -> Option<crate::ui_model::UiFrame> {
            None
        }
    }

    fn feed() -> FeedHandle {
        FeedHandle(std::sync::Arc::new(NoFrames))
    }

    fn target(id: &str, kind: TargetKindView) -> CaptureTargetView {
        CaptureTargetView {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
        }
    }

    fn targets_event() -> UiEvent {
        UiEvent::CaptureTargetsUpdated(vec![
            target("w1", TargetKindView::Window),
            target("s1", TargetKindView::Screen),
        ])
    }

    fn remote_share(state: &mut AppState, identity: &str) {
        state.apply_event(UiEvent::RemoteScreenShareStarted {
            identity: identity.to_owned(),
        });
    }

    #[test]
    fn picker_opens_only_in_a_room_and_requests_targets() {
        let mut state = state_with_code();
        assert!(state.handle_action(UserAction::OpenSharePicker).is_empty());
        assert!(!state.picker.open);
        in_room(&mut state);
        assert_eq!(
            state.handle_action(UserAction::OpenSharePicker),
            vec![UiCommand::RefreshCaptureTargets]
        );
        assert!(state.picker.open);
    }

    #[test]
    fn picker_preselects_first_screen_and_starts_share() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.handle_action(UserAction::OpenSharePicker);
        assert!(state.handle_action(UserAction::StartShare).is_empty());
        state.apply_event(targets_event());
        assert_eq!(state.picker.selected.as_deref(), Some("s1"));
        state.handle_action(UserAction::SelectCaptureTarget("w1".to_owned()));
        state.handle_action(UserAction::SelectCaptureTarget("zzz".to_owned()));
        state.handle_action(UserAction::SelectSharePreset(SharePresetView::P720F30));
        assert_eq!(
            state.handle_action(UserAction::StartShare),
            vec![UiCommand::StartScreenShare {
                target_id: "w1".to_owned(),
                preset: SharePresetView::P720F30
            }]
        );
        assert!(!state.picker.open);
        assert_eq!(room(&state).share.local, LocalShare::Starting);
        assert!(state.handle_action(UserAction::StartShare).is_empty());
        assert!(state.handle_action(UserAction::OpenSharePicker).is_empty());

        state.apply_event(UiEvent::ScreenShareStarted { preview: feed() });
        assert_eq!(room(&state).share.local, LocalShare::Active);
        assert!(room(&state).share.preview.is_some());
        assert_eq!(
            state.handle_action(UserAction::StopShare),
            vec![UiCommand::StopScreenShare]
        );
        state.apply_event(UiEvent::ScreenShareStopped);
        assert_eq!(room(&state).share.local, LocalShare::Idle);
        assert_eq!(room(&state).share.preview, None);
        assert!(state.handle_action(UserAction::StopShare).is_empty());
    }

    #[test]
    fn failed_share_start_returns_to_idle_with_error() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.handle_action(UserAction::OpenSharePicker);
        state.apply_event(targets_event());
        state.handle_action(UserAction::StartShare);
        state.apply_event(UiEvent::ShareFailed {
            message: "falhou".to_owned(),
        });
        let share = &room(&state).share;
        assert_eq!(share.local, LocalShare::Idle);
        assert_eq!(share.error.as_deref(), Some("falhou"));
        state.handle_action(UserAction::DismissShareError);
        assert_eq!(room(&state).share.error, None);
    }

    #[test]
    fn watch_flow_and_fullscreen() {
        let mut state = state_with_code();
        in_room(&mut state);
        assert!(
            state
                .handle_action(UserAction::Watch("ana".to_owned()))
                .is_empty()
        );
        remote_share(&mut state, "ana");
        assert_eq!(
            state.handle_action(UserAction::Watch("ana".to_owned())),
            vec![UiCommand::WatchScreenShare {
                identity: "ana".to_owned()
            }]
        );
        state.apply_event(UiEvent::WatchingStarted {
            identity: "ana".to_owned(),
            feed: feed(),
        });
        assert!(state.is_watching());
        assert!(!state.is_fullscreen());
        state.handle_action(UserAction::ToggleFullscreen);
        assert!(state.is_fullscreen());
        assert!(
            state
                .handle_action(UserAction::Watch("ana".to_owned()))
                .is_empty()
        );
        assert_eq!(
            state.handle_action(UserAction::StopWatching),
            vec![UiCommand::StopWatching]
        );
        assert!(!state.is_watching());
        assert!(!state.is_fullscreen());
        assert!(state.handle_action(UserAction::StopWatching).is_empty());
    }

    #[test]
    fn remote_share_end_or_departure_stops_watching() {
        let mut state = state_with_code();
        in_room(&mut state);
        remote_share(&mut state, "ana");
        state.apply_event(UiEvent::WatchingStarted {
            identity: "ana".to_owned(),
            feed: feed(),
        });
        state.apply_event(UiEvent::RemoteScreenShareStopped {
            identity: "ana".to_owned(),
        });
        assert!(!state.is_watching());
        assert!(!room(&state).share.is_sharing_remote("ana"));

        remote_share(&mut state, "ana");
        state.apply_event(UiEvent::WatchingStarted {
            identity: "ana".to_owned(),
            feed: feed(),
        });
        state.apply_event(UiEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(!state.is_watching());
        assert!(room(&state).share.remote.is_empty());

        remote_share(&mut state, "bia");
        state.apply_event(UiEvent::WatchingStarted {
            identity: "bia".to_owned(),
            feed: feed(),
        });
        state.apply_event(UiEvent::WatchingStopped);
        assert!(!state.is_watching());
    }

    #[test]
    fn rejoining_resets_share_state_and_leaving_closes_picker() {
        let mut state = state_with_code();
        in_room(&mut state);
        remote_share(&mut state, "ana");
        state.apply_event(UiEvent::ScreenShareStarted { preview: feed() });
        state.apply_event(UiEvent::Joined {
            room: "jogos".to_owned(),
            local_identity: "pedro".to_owned(),
            participants: vec![person("ana")],
        });
        assert_eq!(room(&state).share, ShareState::default());

        state.handle_action(UserAction::OpenSharePicker);
        assert!(state.picker.open);
        state.handle_action(UserAction::Leave);
        assert!(!state.picker.open);
    }

    #[test]
    fn aspect_fit_keeps_ratio() {
        assert_eq!(aspect_fit((800.0, 600.0), (1920, 1080)), (800.0, 450.0));
        let (w, h) = aspect_fit((1000.0, 300.0), (1920, 1080));
        assert!((w - 533.333).abs() < 0.01 && (h - 300.0).abs() < 0.01);
        assert_eq!(aspect_fit((0.0, 100.0), (1920, 1080)), (0.0, 0.0));
        assert_eq!(aspect_fit((100.0, 100.0), (0, 0)), (0.0, 0.0));
    }

    #[test]
    fn closing_picker_frees_thumbnails_cancels_targets_and_ignores_late_thumbnails() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.handle_action(UserAction::OpenSharePicker);
        state.apply_event(targets_event());
        state.apply_event(UiEvent::CaptureThumbnail {
            id: "w1".to_owned(),
            width: 2,
            height: 1,
            rgba: vec![9; 8],
        });
        assert_eq!(state.picker.thumbnails.len(), 1);

        let commands = state.handle_action(UserAction::CloseSharePicker);
        assert_eq!(commands, vec![UiCommand::CancelCaptureTargets]);
        assert!(!state.picker.open);
        assert!(state.picker.thumbnails.is_empty());

        state.apply_event(UiEvent::CaptureThumbnail {
            id: "w1".to_owned(),
            width: 2,
            height: 1,
            rgba: vec![7; 8],
        });
        assert!(state.picker.thumbnails.is_empty());

        state.handle_action(UserAction::OpenSharePicker);
        state.apply_event(targets_event());
        state.handle_action(UserAction::StartShare);
        assert!(!state.picker.open);
        state.apply_event(UiEvent::CaptureThumbnail {
            id: "s1".to_owned(),
            width: 2,
            height: 1,
            rgba: vec![5; 8],
        });
        assert!(state.picker.thumbnails.is_empty());
    }

    #[test]
    fn picker_stores_thumbnails_for_current_targets_and_resets_on_refresh() {
        let mut state = state_with_code();
        in_room(&mut state);
        state.handle_action(UserAction::OpenSharePicker);
        let initial_serial = state.picker.targets_serial;
        state.apply_event(targets_event());
        assert_ne!(state.picker.targets_serial, initial_serial);
        let serial_after_list = state.picker.targets_serial;

        state.apply_event(UiEvent::CaptureThumbnail {
            id: "w1".to_owned(),
            width: 2,
            height: 1,
            rgba: vec![9; 8],
        });
        assert_eq!(
            state.picker.thumbnails.get("w1"),
            Some(&ThumbnailView {
                width: 2,
                height: 1,
                rgba: vec![9; 8],
            })
        );

        // Unknown target or invalid payload is ignored.
        state.apply_event(UiEvent::CaptureThumbnail {
            id: "unknown".to_owned(),
            width: 1,
            height: 1,
            rgba: vec![1; 4],
        });
        state.apply_event(UiEvent::CaptureThumbnail {
            id: "s1".to_owned(),
            width: 2,
            height: 2,
            rgba: vec![1; 4],
        });
        assert_eq!(state.picker.thumbnails.len(), 1);

        // Refreshing resets loaded thumbnails cleanly and bumps serial.
        state.handle_action(UserAction::RefreshCaptureTargets);
        assert!(state.picker.thumbnails.is_empty());
        assert!(state.picker.targets_serial > serial_after_list);

        state.apply_event(targets_event());
        assert!(state.picker.thumbnails.is_empty());
    }
}
