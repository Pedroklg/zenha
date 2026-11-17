//! Recepção dos quadros da transmissão remota assistida.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use futures_util::StreamExt;
use livekit::prelude::TrackSid;
use livekit::webrtc::video_frame::{VideoBufferType, VideoFormatType};
use livekit::webrtc::video_stream::native::NativeVideoStream;
use livekit::webrtc::video_track::RtcVideoTrack;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle as TaskHandle;

use crate::backend::BackendEvent;
use crate::backend::support::ShareMeter;
use crate::video::FrameSlot;

struct WatchState {
    identity: String,
    sid: String,
    slot: FrameSlot,
    task: Option<TaskHandle<()>>,
}

/// The single remote screen share being watched, shared by the backend and the event pump.
#[derive(Default)]
pub(crate) struct Watch {
    state: Mutex<Option<WatchState>>,
}

impl Watch {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<WatchState>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
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
            task: None,
        });
        previous
    }

    pub(crate) fn clear(&self) -> Option<String> {
        let state = self.lock().take()?;
        if let Some(task) = state.task {
            task.abort();
        }
        Some(state.identity)
    }

    pub(crate) fn is_watching(&self, identity: &str) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|state| state.identity == identity)
    }

    pub(crate) fn clear_if(&self, identity: &str) -> bool {
        let matches = self
            .lock()
            .as_ref()
            .is_some_and(|state| state.identity == identity);
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

    /// Starts reading frames if `identity` is the one being watched. Needs a Tokio runtime.
    pub(crate) fn start_stream(
        &self,
        identity: &str,
        track: RtcVideoTrack,
        events: UnboundedSender<BackendEvent>,
        ui_visible: Arc<std::sync::atomic::AtomicBool>,
    ) {
        let mut guard = self.lock();
        let Some(state) = guard.as_mut().filter(|state| state.identity == identity) else {
            return;
        };
        if let Some(task) = state.task.take() {
            task.abort();
        }
        state.task = Some(tokio::spawn(read_frames(
            track,
            state.slot.clone(),
            identity.to_owned(),
            events,
            ui_visible,
        )));
    }
}

async fn read_frames(
    track: RtcVideoTrack,
    slot: FrameSlot,
    identity: String,
    events: UnboundedSender<BackendEvent>,
    ui_visible: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut stream = NativeVideoStream::new(track);
    let mut meter = ShareMeter::new(Instant::now());
    let mut size = (0u32, 0u32);
    while let Some(frame) = stream.next().await {
        if !ui_visible.load(std::sync::atomic::Ordering::Relaxed) {
            continue;
        }
        let started = Instant::now();
        let buffer = &frame.buffer;
        if buffer.buffer_type() == VideoBufferType::Native {
            tracing::warn!("received a native video buffer that cannot be read on the CPU");
            continue;
        }
        let (width, height) = (buffer.width(), buffer.height());
        let (Ok(w), Ok(h)) = (i32::try_from(width), i32::try_from(height)) else {
            continue;
        };
        if width == 0 || height == 0 {
            continue;
        }
        let converted = catch_unwind(AssertUnwindSafe(|| {
            slot.push_with(width, height, |rgba| {
                buffer.to_argb(VideoFormatType::ABGR, rgba, width * 4, w, h);
            });
        }));
        if converted.is_err() {
            tracing::warn!("converting a received frame failed, frame skipped");
            continue;
        }
        if (width, height) != size {
            tracing::info!(%identity, width, height, "receiving screen share frames");
            size = (width, height);
        }
        let now = Instant::now();
        meter.record_frame(now.saturating_duration_since(started));
        if let Some(report) = meter.report(now) {
            tracing::info!(
                %identity,
                frames = report.frames,
                avg_convert_ms = format_args!("{:.2}", report.avg_ms),
                window_secs = format_args!("{:.1}", report.window_secs),
                "screen share receive stats"
            );
        }
    }
    let _ = events.send(BackendEvent::WatchEnded {
        identity,
        reason: None,
    });
}
