//! Screen sharing, capture thumbnails and remote viewing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::{JoinHandle, JoinSet};

use crate::api::{Event, SessionError};
use crate::backend::MediaBackend;
use crate::video::{
    CAPTURE_THUMBNAIL_MAX_SIDE, CAPTURE_THUMBNAIL_TIMEOUT, CaptureTarget, FrameSlot, SharePreset,
    VideoFrame,
};

use super::Session;

const THUMBNAIL_CONCURRENCY: usize = 4;

pub(super) struct ThumbnailReady {
    seq: u64,
    id: String,
    frame: VideoFrame,
}

pub(super) struct ThumbnailJob {
    cancel: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for ThumbnailJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}

fn spawn_thumbnails(
    seq: u64,
    targets: Vec<CaptureTarget>,
    backend: Arc<dyn MediaBackend>,
    tx: UnboundedSender<ThumbnailReady>,
) -> ThumbnailJob {
    let cancel = Arc::new(AtomicBool::new(false));
    let job_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        let mut set = JoinSet::new();
        let permits = Arc::new(Semaphore::new(THUMBNAIL_CONCURRENCY));
        for target in targets {
            if job_cancel.load(Ordering::Relaxed) {
                break;
            }
            let Ok(permit) = permits.clone().acquire_owned().await else {
                break;
            };
            if job_cancel.load(Ordering::Relaxed) {
                break;
            }
            let backend = backend.clone();
            let cancel = job_cancel.clone();
            let tx = tx.clone();
            set.spawn(async move {
                let _permit = permit;
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let result = tokio::time::timeout(
                    CAPTURE_THUMBNAIL_TIMEOUT,
                    backend.capture_thumbnail(
                        &target,
                        CAPTURE_THUMBNAIL_MAX_SIDE,
                        CAPTURE_THUMBNAIL_TIMEOUT,
                    ),
                )
                .await;
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                match result {
                    Ok(Ok(frame)) if frame.is_valid() => {
                        let _ = tx.send(ThumbnailReady {
                            seq,
                            id: target.id,
                            frame,
                        });
                    }
                    Ok(Ok(_)) => {
                        tracing::debug!(target_id = %target.id, "captured invalid thumbnail frame");
                    }
                    Ok(Err(err)) => {
                        tracing::debug!(target_id = %target.id, %err, "capture thumbnail failed");
                    }
                    Err(_) => {
                        tracing::debug!(target_id = %target.id, "capture thumbnail timed out");
                    }
                }
            });
        }
        while set.join_next().await.is_some() {}
    });
    ThumbnailJob { cancel, task }
}

impl Session {
    pub(super) async fn reset_video(&mut self) {
        if self.sharing {
            self.sharing = false;
            if let Err(err) = self.backend.stop_screen_share().await {
                tracing::warn!(%err, "backend stop screen share failed");
            }
            self.emit(Event::ScreenShareStopped);
        }
        if self.watching.take().is_some() {
            if let Err(err) = self.backend.stop_watching().await {
                tracing::warn!(%err, "backend stop watching failed");
            }
            self.emit(Event::WatchingStopped);
        }
    }

    pub(super) fn cancel_capture_targets(&mut self) {
        self.capture_targets_seq = self.capture_targets_seq.wrapping_add(1);
        self.thumbnail_job = None;
    }

    pub(super) async fn refresh_capture_targets(&mut self) {
        self.cancel_capture_targets();
        let targets = match self.backend.capture_targets().await {
            Ok(targets) => targets,
            Err(err) => {
                self.emit(Event::CommandFailed(err));
                return;
            }
        };
        self.emit(Event::CaptureTargetsUpdated(targets.clone()));
        if !targets.is_empty() {
            self.thumbnail_job = Some(spawn_thumbnails(
                self.capture_targets_seq,
                targets,
                self.backend.clone(),
                self.thumbnail_tx.clone(),
            ));
        }
    }

    pub(super) fn on_thumbnail(&mut self, ready: ThumbnailReady) {
        if ready.seq != self.capture_targets_seq {
            return;
        }
        self.emit(Event::CaptureThumbnail {
            id: ready.id,
            width: ready.frame.width,
            height: ready.frame.height,
            rgba: ready.frame.rgba,
        });
    }

    pub(super) async fn start_share(&mut self, target_id: &str, preset: SharePreset) {
        if !self.connected() {
            self.emit(Event::CommandFailed(SessionError::NotInRoom));
            return;
        }
        if self.sharing {
            self.emit(Event::CommandFailed(SessionError::ShareActive));
            return;
        }
        self.cancel_capture_targets();
        let preview = FrameSlot::with_waker(self.sink.waker());
        match self
            .backend
            .start_screen_share(target_id, preset, preview.clone())
            .await
        {
            Ok(()) => {
                self.sharing = true;
                self.emit(Event::ScreenShareStarted { preview });
            }
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) async fn stop_share(&mut self) {
        if !self.sharing {
            return;
        }
        self.sharing = false;
        if let Err(err) = self.backend.stop_screen_share().await {
            self.emit(Event::CommandFailed(err));
        }
        self.emit(Event::ScreenShareStopped);
    }

    pub(super) async fn watch(&mut self, identity: String) {
        if !self.connected() {
            self.emit(Event::CommandFailed(SessionError::NotInRoom));
            return;
        }
        if self.watching.as_deref() == Some(identity.as_str()) {
            return;
        }
        let frames = FrameSlot::with_waker(self.sink.waker());
        match self
            .backend
            .watch_screen_share(&identity, frames.clone())
            .await
        {
            Ok(()) => {
                self.watching = Some(identity.clone());
                self.emit(Event::WatchingStarted { identity, frames });
            }
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) async fn stop_watch(&mut self) {
        if self.watching.take().is_none() {
            return;
        }
        if let Err(err) = self.backend.stop_watching().await {
            self.emit(Event::CommandFailed(err));
        }
        self.emit(Event::WatchingStopped);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::api::{Command, Event, SessionError};
    use crate::backend::BackendEvent;
    use crate::testing::{BackendCall, FakeThumbnail};
    use crate::video::capture::CaptureEnd;
    use crate::video::{CaptureKind, CaptureTarget, FrameSlot, SharePreset, VideoFrame};

    use super::super::test_support::{Harness, harness};

    fn target(id: &str) -> CaptureTarget {
        CaptureTarget {
            id: id.to_owned(),
            name: id.to_uppercase(),
            kind: CaptureKind::Screen,
        }
    }

    fn start_share(h: &Harness, id: &str) {
        h.send(Command::StartScreenShare {
            target_id: id.to_owned(),
            preset: SharePreset::P720F30,
        });
    }

    fn frame(value: u8) -> VideoFrame {
        VideoFrame {
            width: 1,
            height: 1,
            rgba: vec![value; 4],
        }
    }

    async fn watch(h: &mut Harness, identity: &str) -> FrameSlot {
        h.send(Command::WatchScreenShare {
            identity: identity.to_owned(),
        });
        let Event::WatchingStarted {
            identity: got,
            frames,
        } = h.next().await
        else {
            panic!("expected WatchingStarted");
        };
        assert_eq!(got, identity);
        frames
    }

    #[tokio::test(start_paused = true)]
    async fn capture_targets_are_listed_even_outside_a_room() {
        let mut h = harness();
        h.backend.set_capture_targets(vec![target("s1")]);
        h.send(Command::RefreshCaptureTargets);
        assert_eq!(
            h.next().await,
            Event::CaptureTargetsUpdated(vec![target("s1")])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_capture_targets_emits_thumbnails_and_skips_failures_and_timeouts() {
        let mut h = harness();
        h.backend
            .set_capture_targets(vec![target("s1"), target("fail"), target("slow")]);
        h.backend
            .set_thumbnail("s1", FakeThumbnail::Ready(frame(42)));
        h.backend.set_thumbnail(
            "fail",
            FakeThumbnail::Fail(SessionError::Video("boom".to_owned())),
        );
        h.backend.set_thumbnail(
            "slow",
            FakeThumbnail::Delay {
                delay: Duration::from_secs(5),
                frame: frame(99),
            },
        );
        h.send(Command::RefreshCaptureTargets);
        assert_eq!(
            h.next().await,
            Event::CaptureTargetsUpdated(vec![target("s1"), target("fail"), target("slow")])
        );
        assert_eq!(
            h.next().await,
            Event::CaptureThumbnail {
                id: "s1".to_owned(),
                width: 1,
                height: 1,
                rgba: vec![42; 4],
            }
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(h.events.try_recv().ok(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn newer_capture_target_list_discards_stale_thumbnails() {
        let mut h = harness();
        h.backend.set_capture_targets(vec![target("old")]);
        h.backend.set_thumbnail(
            "old",
            FakeThumbnail::Delay {
                delay: Duration::from_millis(200),
                frame: frame(1),
            },
        );
        h.send(Command::RefreshCaptureTargets);
        assert_eq!(
            h.next().await,
            Event::CaptureTargetsUpdated(vec![target("old")])
        );

        h.backend.set_capture_targets(vec![target("new")]);
        h.backend
            .set_thumbnail("new", FakeThumbnail::Ready(frame(7)));
        h.send(Command::RefreshCaptureTargets);
        assert_eq!(
            h.next().await,
            Event::CaptureTargetsUpdated(vec![target("new")])
        );
        assert_eq!(
            h.next().await,
            Event::CaptureThumbnail {
                id: "new".to_owned(),
                width: 1,
                height: 1,
                rgba: vec![7; 4],
            }
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(h.events.try_recv().ok(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn start_share_delivers_preview_slot_until_stopped() {
        let mut h = harness();
        h.join("geral").await;
        start_share(&h, "s1");
        let Event::ScreenShareStarted { preview } = h.next().await else {
            panic!("expected ScreenShareStarted");
        };
        assert!(h.backend.push_share_preview_frame(frame(11)));
        let mut seen = 0;
        let got = preview.take_if_newer(&mut seen).unwrap();
        assert_eq!(got.rgba, vec![11; 4]);
        h.send(Command::StopScreenShare);
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert!(!h.backend.push_share_preview_frame(frame(12)));
    }

    #[tokio::test(start_paused = true)]
    async fn start_share_reaches_the_backend_and_second_start_is_rejected() {
        let mut h = harness();
        start_share(&h, "s1");
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::NotInRoom)
        );
        h.join("geral").await;
        start_share(&h, "s1");
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
        assert_eq!(
            h.backend.sharing(),
            Some(("s1".to_owned(), SharePreset::P720F30))
        );
        start_share(&h, "s2");
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::ShareActive)
        );
        assert_eq!(h.backend.sharing().unwrap().0, "s1");
        h.send(Command::StopScreenShare);
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.backend.sharing(), None);
        h.send(Command::StopScreenShare);
        start_share(&h, "s2");
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_start_reports_the_error_and_allows_a_retry() {
        let mut h = harness();
        h.join("geral").await;
        h.backend
            .fail_next_share(SessionError::TargetNotFound("s1".to_owned()));
        start_share(&h, "s1");
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::TargetNotFound("s1".to_owned()))
        );
        start_share(&h, "s1");
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn share_stops_on_leave_and_on_room_switch() {
        let mut h = harness();
        h.join("geral").await;
        start_share(&h, "s1");
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
        h.send(Command::Leave);
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.next().await, Event::Left);
        let calls = h.backend.calls();
        let stop = calls
            .iter()
            .position(|c| *c == BackendCall::StopScreenShare)
            .unwrap();
        let disconnect = calls
            .iter()
            .position(|c| *c == BackendCall::Disconnect)
            .unwrap();
        assert!(stop < disconnect);

        h.join("geral").await;
        start_share(&h, "s1");
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
        h.send(Command::Join {
            room: "jogos".to_owned(),
        });
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { .. }));
        assert_eq!(h.backend.sharing(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn capture_that_ends_by_itself_reports_why_and_unpublishes() {
        let mut h = harness();
        h.join("geral").await;
        start_share(&h, "s1");
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
        h.backend.emit(BackendEvent::ScreenShareEnded {
            reason: CaptureEnd::TargetClosed,
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::CaptureEnded(CaptureEnd::TargetClosed))
        );
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.backend.sharing(), None);
        h.backend.emit(BackendEvent::ScreenShareEnded {
            reason: CaptureEnd::TargetClosed,
        });
        h.send(Command::RefreshCaptureTargets);
        assert!(matches!(h.next().await, Event::CaptureTargetsUpdated(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn remote_share_events_are_forwarded() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.emit(BackendEvent::RemoteScreenShareStarted {
            identity: "ana".to_owned(),
        });
        h.backend.emit(BackendEvent::RemoteScreenShareStopped {
            identity: "ana".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::RemoteScreenShareStarted {
                identity: "ana".to_owned()
            }
        );
        assert_eq!(
            h.next().await,
            Event::RemoteScreenShareStopped {
                identity: "ana".to_owned()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn watch_switch_and_stop_reach_the_backend_and_frames_arrive() {
        let mut h = harness();
        h.send(Command::WatchScreenShare {
            identity: "ana".to_owned(),
        });
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::NotInRoom)
        );
        h.join("geral").await;
        let slot_a = watch(&mut h, "ana").await;
        assert_eq!(h.backend.watching().as_deref(), Some("ana"));
        assert!(h.backend.push_frame(frame(1)));
        let mut seen = 0;
        assert_eq!(slot_a.take_if_newer(&mut seen).unwrap().rgba[0], 1);

        h.send(Command::WatchScreenShare {
            identity: "ana".to_owned(),
        });
        let slot_b = watch(&mut h, "bruno").await;
        assert_ne!(slot_a, slot_b);
        assert_eq!(h.backend.watching().as_deref(), Some("bruno"));
        assert!(h.backend.push_frame(frame(2)));
        assert!(slot_a.take_if_newer(&mut seen).is_none());
        let mut seen_b = 0;
        assert_eq!(slot_b.take_if_newer(&mut seen_b).unwrap().rgba[0], 2);

        h.send(Command::StopWatching);
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(h.backend.watching(), None);
        assert!(!h.backend.push_frame(frame(3)));
        let watches = h
            .backend
            .calls()
            .into_iter()
            .filter(|c| matches!(c, BackendCall::WatchScreenShare { .. }))
            .count();
        assert_eq!(watches, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn watching_ends_when_the_share_stops_the_watcher_leaves_or_the_room_is_left() {
        let mut h = harness();
        h.join("geral").await;
        watch(&mut h, "ana").await;
        h.backend.emit(BackendEvent::RemoteScreenShareStopped {
            identity: "ana".to_owned(),
        });
        assert!(matches!(
            h.next().await,
            Event::RemoteScreenShareStopped { .. }
        ));
        assert_eq!(h.next().await, Event::WatchingStopped);

        watch(&mut h, "ana").await;
        h.backend.emit(BackendEvent::ParticipantLeft {
            identity: "ana".to_owned(),
        });
        assert!(matches!(h.next().await, Event::ParticipantLeft { .. }));
        assert_eq!(h.next().await, Event::WatchingStopped);

        watch(&mut h, "bruno").await;
        h.backend.emit(BackendEvent::WatchEnded {
            identity: "bruno".to_owned(),
            reason: Some("boom".to_owned()),
        });
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::Video("boom".to_owned()))
        );

        watch(&mut h, "bruno").await;
        h.send(Command::Leave);
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(h.next().await, Event::Left);
    }

    #[tokio::test(start_paused = true)]
    async fn unexpected_disconnect_stops_share_and_watch() {
        let mut h = harness();
        h.join("geral").await;
        start_share(&h, "s1");
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
        watch(&mut h, "ana").await;
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::ScreenShareStopped);
        assert_eq!(h.next().await, Event::WatchingStopped);
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_capture_targets_and_start_share_stop_pending_thumbnails() {
        let mut h = harness();
        h.backend.set_capture_targets(vec![target("slow")]);
        h.backend.set_thumbnail(
            "slow",
            FakeThumbnail::Delay {
                delay: Duration::from_secs(1),
                frame: frame(99),
            },
        );
        h.send(Command::RefreshCaptureTargets);
        assert_eq!(
            h.next().await,
            Event::CaptureTargetsUpdated(vec![target("slow")])
        );
        h.send(Command::CancelCaptureTargets);
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(h.events.try_recv().ok(), None);

        h.join("geral").await;
        h.send(Command::RefreshCaptureTargets);
        assert_eq!(
            h.next().await,
            Event::CaptureTargetsUpdated(vec![target("slow")])
        );
        h.send(Command::StartScreenShare {
            target_id: "slow".to_owned(),
            preset: SharePreset::P720F30,
        });
        assert!(matches!(h.next().await, Event::ScreenShareStarted { .. }));
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(h.events.try_recv().ok(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn ui_visibility_hides_and_resumes_watch_and_preview_frames() {
        let mut h = harness();
        h.join("geral").await;
        h.backend.set_capture_targets(vec![target("s1")]);
        h.send(Command::StartScreenShare {
            target_id: "s1".to_owned(),
            preset: SharePreset::P720F30,
        });
        let Event::ScreenShareStarted { preview } = h.next().await else {
            panic!("expected ScreenShareStarted");
        };
        let watch_slot = watch(&mut h, "ana").await;

        let mut seen_preview = 0;
        let mut seen_watch = 0;
        assert!(h.backend.push_share_preview_frame(frame(1)));
        assert!(h.backend.push_frame(frame(2)));
        assert_eq!(preview.take_if_newer(&mut seen_preview).unwrap().rgba[0], 1);
        assert_eq!(
            watch_slot.take_if_newer(&mut seen_watch).unwrap().rgba[0],
            2
        );

        h.send(Command::SetUiVisible(false));
        h.send(Command::RefreshRooms);
        assert!(matches!(h.next().await, Event::RoomsUpdated(_)));
        assert!(!h.backend.push_share_preview_frame(frame(3)));
        assert!(!h.backend.push_frame(frame(4)));
        assert!(preview.take_if_newer(&mut seen_preview).is_none());
        assert!(watch_slot.take_if_newer(&mut seen_watch).is_none());
        assert_eq!(h.backend.watching().as_deref(), Some("ana"));

        h.send(Command::SetUiVisible(true));
        h.send(Command::RefreshRooms);
        assert!(matches!(h.next().await, Event::RoomsUpdated(_)));
        assert!(h.backend.push_share_preview_frame(frame(5)));
        assert!(h.backend.push_frame(frame(6)));
        assert_eq!(preview.take_if_newer(&mut seen_preview).unwrap().rgba[0], 5);
        assert_eq!(
            watch_slot.take_if_newer(&mut seen_watch).unwrap().rgba[0],
            6
        );
    }
}
