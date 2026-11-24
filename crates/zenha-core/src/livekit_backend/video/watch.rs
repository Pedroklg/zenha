//! Reception of frames from the watched remote stream, switching between SFU and P2P without a gap.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use futures_util::StreamExt;
use livekit::prelude::TrackSid;
use livekit::webrtc::video_frame::{BoxVideoBuffer, VideoBufferType, VideoFormatType};
use livekit::webrtc::video_stream::native::NativeVideoStream;
use livekit::webrtc::video_track::RtcVideoTrack;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle as TaskHandle;

use crate::backend::BackendEvent;
use crate::backend::support::ShareMeter;
use crate::video::FrameSlot;

/// Both readers write to the same `FrameSlot`; the new one takes over on its first frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchMode {
    Sfu,
    UpgradingToP2p,
    P2p,
    DowngradingToSfu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Sfu,
    P2p,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sfu => "sfu",
            Self::P2p => "p2p",
        }
    }
}

struct WatchState {
    identity: String,
    sid: String,
    slot: FrameSlot,
    mode: WatchMode,
    sfu_gen: u64,
    sfu_task: Option<TaskHandle<()>>,
    p2p_gen: u64,
    p2p_task: Option<TaskHandle<()>>,
    last_p2p_frame: Option<Instant>,
}

impl WatchState {
    fn stop_p2p(&mut self) {
        if let Some(task) = self.p2p_task.take() {
            task.abort();
        }
        self.mode = WatchMode::Sfu;
        self.last_p2p_frame = None;
    }
}

/// The single remote screen share being watched, shared by the backend, event pump and P2P coordinator.
#[derive(Default)]
pub(crate) struct Watch {
    state: Mutex<Option<WatchState>>,
}

impl Watch {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<WatchState>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with<R>(&self, identity: &str, f: impl FnOnce(&mut WatchState) -> R) -> Option<R> {
        self.lock()
            .as_mut()
            .filter(|state| state.identity == identity)
            .map(f)
    }

    /// Starts watching `identity`; returns the identity watched before, if any.
    pub(crate) fn replace(
        &self,
        identity: &str,
        sid: &TrackSid,
        slot: FrameSlot,
    ) -> Option<String> {
        let previous = self.clear();
        *self.lock() = Some(WatchState {
            identity: identity.to_owned(),
            sid: sid.as_str().to_owned(),
            slot,
            mode: WatchMode::Sfu,
            sfu_gen: 0,
            sfu_task: None,
            p2p_gen: 0,
            p2p_task: None,
            last_p2p_frame: None,
        });
        previous
    }

    pub(crate) fn clear(&self) -> Option<String> {
        let mut state = self.lock().take()?;
        if let Some(task) = state.sfu_task.take() {
            task.abort();
        }
        state.stop_p2p();
        Some(state.identity)
    }

    pub(crate) fn is_watching(&self, identity: &str) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|state| state.identity == identity)
    }

    pub(crate) fn watched_identity(&self) -> Option<String> {
        self.lock().as_ref().map(|state| state.identity.clone())
    }

    /// Hiding the window stops the SFU reader only; the P2P link keeps running so it survives.
    pub(crate) fn pause_stream(&self) {
        if let Some(state) = self.lock().as_mut()
            && let Some(task) = state.sfu_task.take()
        {
            task.abort();
        }
    }

    pub(crate) fn clear_if(&self, identity: &str) -> bool {
        let matches = self.is_watching(identity);
        if matches {
            self.clear();
        }
        matches
    }

    pub(crate) fn failed_sid_matches(&self, identity: &str, sid: &TrackSid) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|state| state.identity == identity && state.sid == sid.as_str())
    }

    /// True while P2P alone feeds the picture, so showing the window must not resubscribe the SFU.
    pub(crate) fn uses_p2p(&self) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|state| state.mode == WatchMode::P2p)
    }

    pub(crate) fn p2p_status(&self, identity: &str) -> Option<(WatchMode, Option<Instant>)> {
        self.lock()
            .as_ref()
            .filter(|state| state.identity == identity)
            .map(|state| (state.mode, state.last_p2p_frame))
    }

    /// Starts reading SFU frames if `identity` is the one being watched and P2P does not own it.
    pub(crate) fn start_stream(
        self: &Arc<Self>,
        identity: &str,
        track: RtcVideoTrack,
        events: UnboundedSender<BackendEvent>,
        ui_visible: Arc<AtomicBool>,
    ) {
        self.with(identity, |state| {
            if state.mode == WatchMode::P2p {
                return;
            }
            if let Some(task) = state.sfu_task.take() {
                task.abort();
            }
            state.sfu_gen = state.sfu_gen.wrapping_add(1);
            state.sfu_task = Some(tokio::spawn(read_frames(
                track,
                Reader {
                    watch: Arc::clone(self),
                    identity: identity.to_owned(),
                    slot: state.slot.clone(),
                    transport: Transport::Sfu,
                    generation: state.sfu_gen,
                    events,
                    ui_visible,
                },
            )));
        });
    }

    /// Reads the P2P track next to the SFU one; the SFU reader stops on the first P2P frame.
    pub(crate) fn start_p2p_stream(
        self: &Arc<Self>,
        identity: &str,
        track: RtcVideoTrack,
        events: UnboundedSender<BackendEvent>,
        ui_visible: Arc<AtomicBool>,
    ) -> bool {
        self.with(identity, |state| {
            if !matches!(state.mode, WatchMode::Sfu | WatchMode::UpgradingToP2p) {
                return false;
            }
            state.stop_p2p();
            state.p2p_gen = state.p2p_gen.wrapping_add(1);
            state.mode = WatchMode::UpgradingToP2p;
            state.p2p_task = Some(tokio::spawn(read_frames(
                track,
                Reader {
                    watch: Arc::clone(self),
                    identity: identity.to_owned(),
                    slot: state.slot.clone(),
                    transport: Transport::P2p,
                    generation: state.p2p_gen,
                    events,
                    ui_visible,
                },
            )));
            true
        })
        .unwrap_or(false)
    }

    /// Planned downgrade: P2P keeps the picture until the SFU reader delivers its first frame.
    pub(crate) fn begin_downgrade(&self, identity: &str) -> bool {
        self.with(identity, |state| {
            let ok = state.mode == WatchMode::P2p;
            if ok {
                state.mode = WatchMode::DowngradingToSfu;
            }
            ok
        })
        .unwrap_or(false)
    }

    /// Drops the P2P reader; true when no SFU reader is left and the SFU must be subscribed again.
    pub(crate) fn end_p2p(&self, identity: &str) -> bool {
        self.with(identity, |state| {
            state.stop_p2p();
            state.sfu_task.is_none()
        })
        .unwrap_or(false)
    }

    fn frame_arrived(&self, identity: &str, transport: Transport, generation: u64) {
        self.with(identity, |state| match transport {
            Transport::P2p if state.p2p_gen == generation => {
                state.last_p2p_frame = Some(Instant::now());
                if state.mode == WatchMode::UpgradingToP2p {
                    if let Some(task) = state.sfu_task.take() {
                        task.abort();
                    }
                    state.mode = WatchMode::P2p;
                }
            }
            Transport::Sfu
                if state.sfu_gen == generation && state.mode == WatchMode::DowngradingToSfu =>
            {
                state.stop_p2p();
            }
            _ => {}
        });
    }

    fn should_emit_watch_ended(&self, identity: &str, sfu_gen: u64) -> bool {
        self.lock().as_ref().is_some_and(|state| {
            state.identity == identity && state.sfu_gen == sfu_gen && state.mode == WatchMode::Sfu
        })
    }
}

struct Reader {
    watch: Arc<Watch>,
    identity: String,
    slot: FrameSlot,
    transport: Transport,
    generation: u64,
    events: UnboundedSender<BackendEvent>,
    ui_visible: Arc<AtomicBool>,
}

async fn read_frames(track: RtcVideoTrack, reader: Reader) {
    let Reader {
        watch,
        identity,
        slot,
        transport,
        generation,
        events,
        ui_visible,
    } = reader;
    let mut stream = NativeVideoStream::new(track);
    let mut meter = ShareMeter::new(Instant::now());
    let mut size = (0u32, 0u32);
    while let Some(frame) = stream.next().await {
        // P2P frames count as alive even while hidden, so the stall watchdog does not fire.
        if transport == Transport::P2p {
            watch.frame_arrived(&identity, transport, generation);
        }
        if !ui_visible.load(Ordering::Relaxed) {
            continue;
        }
        let started = Instant::now();
        let Some((width, height)) = push_decoded_frame(&frame.buffer, &slot) else {
            continue;
        };
        if transport == Transport::Sfu {
            watch.frame_arrived(&identity, transport, generation);
        }
        if (width, height) != size {
            tracing::info!(%identity, transport = transport.as_str(), width, height, "receiving screen share frames");
            size = (width, height);
        }
        let now = Instant::now();
        meter.record_frame(now.saturating_duration_since(started));
        if let Some(report) = meter.report(now) {
            tracing::info!(
                %identity,
                transport = transport.as_str(),
                frames = report.frames,
                avg_convert_ms = format_args!("{:.2}", report.avg_ms),
                window_secs = format_args!("{:.1}", report.window_secs),
                "screen share receive stats"
            );
        }
    }
    // A P2P track that ends is caught by the coordinator's stall watchdog.
    if transport == Transport::Sfu
        && ui_visible.load(Ordering::Acquire)
        && watch.should_emit_watch_ended(&identity, generation)
    {
        let _ = events.send(BackendEvent::WatchEnded {
            identity,
            reason: None,
        });
    }
}

fn push_decoded_frame(buffer: &BoxVideoBuffer, slot: &FrameSlot) -> Option<(u32, u32)> {
    if buffer.buffer_type() == VideoBufferType::Native {
        tracing::warn!("received a native video buffer that cannot be read on the CPU");
        return None;
    }
    let (width, height) = (buffer.width(), buffer.height());
    let (Ok(w), Ok(h)) = (i32::try_from(width), i32::try_from(height)) else {
        return None;
    };
    if width == 0 || height == 0 {
        return None;
    }
    let converted = catch_unwind(AssertUnwindSafe(|| {
        slot.push_with(width, height, |rgba| {
            buffer.to_argb(VideoFormatType::ABGR, rgba, width * 4, w, h);
        });
    }));
    if converted.is_err() {
        tracing::warn!("converting a received frame failed, frame skipped");
        return None;
    }
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(id: &str) -> TrackSid {
        TrackSid::try_from(id.to_owned()).unwrap()
    }

    fn mode(watch: &Watch) -> Option<WatchMode> {
        watch.p2p_status("alice").map(|(mode, _)| mode)
    }

    fn set(watch: &Watch, f: impl FnOnce(&mut WatchState)) {
        watch.with("alice", f);
    }

    #[test]
    fn first_frame_of_the_new_transport_takes_over_and_guards_watch_ended() {
        let watch = Watch::default();
        assert_eq!(watch.replace("alice", &sid("TR_1"), FrameSlot::new()), None);
        assert_eq!(mode(&watch), Some(WatchMode::Sfu));
        assert!(watch.should_emit_watch_ended("alice", 0));
        assert!(!watch.begin_downgrade("alice"));

        set(&watch, |state| {
            state.mode = WatchMode::UpgradingToP2p;
            state.p2p_gen = 3;
        });
        assert!(!watch.should_emit_watch_ended("alice", 0));
        watch.frame_arrived("alice", Transport::P2p, 2);
        assert_eq!(mode(&watch), Some(WatchMode::UpgradingToP2p));
        watch.frame_arrived("alice", Transport::P2p, 3);
        assert_eq!(mode(&watch), Some(WatchMode::P2p));
        assert!(watch.uses_p2p());
        assert!(watch.p2p_status("alice").and_then(|(_, at)| at).is_some());
        assert_eq!(watch.p2p_status("bob"), None);

        assert!(watch.begin_downgrade("alice"));
        assert!(!watch.uses_p2p());
        set(&watch, |state| state.sfu_gen = 5);
        watch.frame_arrived("alice", Transport::Sfu, 4);
        assert_eq!(mode(&watch), Some(WatchMode::DowngradingToSfu));
        watch.frame_arrived("alice", Transport::Sfu, 5);
        assert_eq!(mode(&watch), Some(WatchMode::Sfu));
        assert!(watch.should_emit_watch_ended("alice", 5));
        assert!(!watch.should_emit_watch_ended("alice", 4));
    }

    #[test]
    fn ending_p2p_reports_whether_the_sfu_must_come_back() {
        let watch = Watch::default();
        watch.replace("alice", &sid("TR_1"), FrameSlot::new());
        set(&watch, |state| state.mode = WatchMode::P2p);
        assert!(!watch.end_p2p("bob"));
        assert_eq!(mode(&watch), Some(WatchMode::P2p));
        assert!(watch.end_p2p("alice"));
        assert_eq!(mode(&watch), Some(WatchMode::Sfu));
        assert!(watch.failed_sid_matches("alice", &sid("TR_1")));
        assert_eq!(watch.clear().as_deref(), Some("alice"));
        assert!(!watch.end_p2p("alice"));
    }
}
