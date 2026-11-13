use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoding};
use livekit::prelude::{LocalTrack, LocalVideoTrack, Room, TrackSid, TrackSource};
use livekit::webrtc::desktop_capturer::{
    CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions, DesktopFrame,
};
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::video_frame::{
    I420Buffer, VideoBufferType, VideoFormatType, VideoFrame as RtcFrame, VideoRotation,
};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::{RtcVideoSource, VideoResolution};
use livekit::webrtc::video_stream::native::NativeVideoStream;
use livekit::webrtc::video_track::RtcVideoTrack;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::task::JoinHandle as TaskHandle;
use tokio::time::timeout;

use crate::api::SessionError;
use crate::backend::BackendEvent;
use crate::backend_support::{CaptureHealth, ShareMeter, parse_target_id, target_id, target_name};
use crate::capture::{BgraFrame, CaptureEnd, FrameSink, FrameSource, Pacer};
use crate::video::{CaptureKind, CaptureTarget, FrameSlot, SharePreset, fit_within};

const SHARE_TRACK_NAME: &str = "screen";
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(15);
const UNPUBLISH_TIMEOUT: Duration = Duration::from_secs(5);

struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// `FrameSource` over the libwebrtc `DesktopCapturer`, polled from its own thread.
#[derive(Default)]
pub(crate) struct DesktopCaptureSource {
    worker: Option<Worker>,
}

impl DesktopCaptureSource {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl Drop for DesktopCaptureSource {
    fn drop(&mut self) {
        self.stop();
    }
}

fn source_type(kind: CaptureKind) -> DesktopCaptureSourceType {
    match kind {
        CaptureKind::Screen => DesktopCaptureSourceType::Screen,
        CaptureKind::Window => DesktopCaptureSourceType::Window,
    }
}

impl FrameSource for DesktopCaptureSource {
    fn list_targets(&mut self) -> Result<Vec<CaptureTarget>, SessionError> {
        let mut targets = Vec::new();
        for kind in [CaptureKind::Screen, CaptureKind::Window] {
            let Some(capturer) =
                DesktopCapturer::new(DesktopCapturerOptions::new(source_type(kind)))
            else {
                tracing::warn!(?kind, "could not create a desktop capturer to list sources");
                continue;
            };
            let mut listed: Vec<CaptureTarget> = capturer
                .get_source_list()
                .iter()
                .enumerate()
                .filter_map(|(index, source)| {
                    Some(CaptureTarget {
                        id: target_id(kind, source.id()),
                        name: target_name(&source.title(), kind, index)?,
                        kind,
                    })
                })
                .collect();
            if kind == CaptureKind::Window {
                listed.sort_by_key(|target| target.name.to_lowercase());
            }
            targets.extend(listed);
        }
        Ok(targets)
    }

    fn start(
        &mut self,
        target: &CaptureTarget,
        fps: u32,
        sink: Box<dyn FrameSink>,
    ) -> Result<(), SessionError> {
        if self.worker.is_some() {
            return Err(SessionError::ShareActive);
        }
        let (kind, source_id) = parse_target_id(&target.id)
            .ok_or_else(|| SessionError::TargetNotFound(target.id.clone()))?;
        if fps == 0 {
            return Err(SessionError::Video("fps must be positive".to_owned()));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("screen-capture".to_owned())
            .spawn(move || capture_loop(kind, source_id, fps, sink, &flag))
            .map_err(|err| SessionError::Internal(err.to_string()))?;
        self.worker = Some(Worker { stop, thread });
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.stop.store(true, Ordering::SeqCst);
            if worker.thread.join().is_err() {
                tracing::error!("capture thread panicked");
            }
        }
    }
}

#[derive(Default)]
struct CallbackState {
    good_frame: AtomicBool,
    temporary_error: AtomicBool,
    permanent_error: AtomicBool,
}

type SharedSink = Arc<Mutex<Box<dyn FrameSink>>>;

fn lock_sink(sink: &SharedSink) -> std::sync::MutexGuard<'_, Box<dyn FrameSink>> {
    sink.lock().unwrap_or_else(PoisonError::into_inner)
}

fn deliver(sink: &SharedSink, state: &CallbackState, frame: &DesktopFrame) {
    let (Ok(width), Ok(height)) = (u32::try_from(frame.width()), u32::try_from(frame.height()))
    else {
        state.temporary_error.store(true, Ordering::SeqCst);
        return;
    };
    let bgra = BgraFrame {
        width,
        height,
        stride: frame.stride() as usize,
        data: frame.data(),
    };
    if !bgra.is_consistent() {
        state.temporary_error.store(true, Ordering::SeqCst);
        return;
    }
    // A panic must not unwind into the C++ capturer that invoked this callback.
    let outcome = catch_unwind(AssertUnwindSafe(|| lock_sink(sink).frame(bgra)));
    if outcome.is_err() {
        state.permanent_error.store(true, Ordering::SeqCst);
    } else {
        state.good_frame.store(true, Ordering::SeqCst);
    }
}

fn capture_loop(
    kind: CaptureKind,
    source_id: u64,
    fps: u32,
    sink: Box<dyn FrameSink>,
    stop: &AtomicBool,
) {
    let sink: SharedSink = Arc::new(Mutex::new(sink));
    let end = |reason: CaptureEnd| lock_sink(&sink).ended(reason);

    let mut options = DesktopCapturerOptions::new(source_type(kind));
    options.set_include_cursor(true);
    let Some(mut capturer) = DesktopCapturer::new(options) else {
        end(CaptureEnd::Failed(
            "could not create the desktop capturer".to_owned(),
        ));
        return;
    };
    let Some(source) = capturer
        .get_source_list()
        .into_iter()
        .find(|source| source.id() == source_id)
    else {
        end(CaptureEnd::TargetClosed);
        return;
    };

    let state = Arc::new(CallbackState::default());
    let callback_sink = sink.clone();
    let callback_state = state.clone();
    capturer.start_capture(Some(source), move |result| match result {
        Ok(frame) => deliver(&callback_sink, &callback_state, &frame),
        Err(CaptureError::Temporary) => {
            callback_state.temporary_error.store(true, Ordering::SeqCst)
        }
        Err(CaptureError::Permanent) => {
            callback_state.permanent_error.store(true, Ordering::SeqCst)
        }
    });

    let mut pacer = Pacer::new(fps, Instant::now());
    let mut health = CaptureHealth::default();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(pacer.until_next(Instant::now()));
        if stop.load(Ordering::SeqCst) {
            break;
        }
        state.good_frame.store(false, Ordering::SeqCst);
        state.temporary_error.store(false, Ordering::SeqCst);
        capturer.capture_frame();
        let now = Instant::now();
        if state.permanent_error.load(Ordering::SeqCst) {
            end(CaptureEnd::TargetClosed);
            return;
        }
        if state.good_frame.load(Ordering::SeqCst) {
            health.good_frame();
        } else if state.temporary_error.load(Ordering::SeqCst) && health.temporary_error(now) {
            end(CaptureEnd::RepeatedErrors);
            return;
        }
        let skipped = pacer.finish_slot(Instant::now());
        if skipped > 0 {
            lock_sink(&sink).dropped(skipped);
        }
    }
}

/// BGRA (B, G, R, A in memory, which libyuv calls ARGB) to I420, scaled when `out` differs.
fn bgra_to_i420(frame: &BgraFrame<'_>, out: (u32, u32)) -> Option<I420Buffer> {
    let width = i32::try_from(frame.width).ok()?;
    let height = i32::try_from(frame.height).ok()?;
    let stride = u32::try_from(frame.stride).ok()?;
    let mut buffer = I420Buffer::new(frame.width, frame.height);
    let (stride_y, stride_u, stride_v) = buffer.strides();
    let (data_y, data_u, data_v) = buffer.data_mut();
    yuv_helper::argb_to_i420(
        frame.data, stride, data_y, stride_y, data_u, stride_u, data_v, stride_v, width, height,
    );
    if out == (frame.width, frame.height) {
        return Some(buffer);
    }
    Some(buffer.scale(i32::try_from(out.0).ok()?, i32::try_from(out.1).ok()?))
}

pub(crate) struct PublishSink {
    source: Arc<OnceLock<NativeVideoSource>>,
    first_size: Option<oneshot::Sender<(u32, u32)>>,
    preset: SharePreset,
    events: UnboundedSender<BackendEvent>,
    meter: ShareMeter,
    published: (u32, u32),
}

impl FrameSink for PublishSink {
    fn frame(&mut self, frame: BgraFrame<'_>) {
        let started = Instant::now();
        if !frame.is_consistent() {
            return;
        }
        if let Some(first) = self.first_size.take() {
            let _ = first.send((frame.width, frame.height));
        }
        let Some(source) = self.source.get() else {
            return;
        };
        let out = fit_within(
            frame.width,
            frame.height,
            self.preset.max_width(),
            self.preset.max_height(),
        );
        let Some(buffer) = bgra_to_i420(&frame, out) else {
            return;
        };
        source.capture_frame(&RtcFrame::new(VideoRotation::VideoRotation0, buffer));
        if out != self.published {
            tracing::info!(
                source_width = frame.width,
                source_height = frame.height,
                published_width = out.0,
                published_height = out.1,
                "screen share frame size"
            );
            self.published = out;
        }
        let now = Instant::now();
        self.meter
            .record_frame(now.saturating_duration_since(started));
        if let Some(report) = self.meter.report(now) {
            tracing::info!(
                frames = report.frames,
                dropped = report.dropped,
                avg_capture_convert_ms = format_args!("{:.2}", report.avg_ms),
                window_secs = format_args!("{:.1}", report.window_secs),
                "screen share capture stats"
            );
        }
    }

    fn dropped(&mut self, count: u64) {
        self.meter.record_dropped(count);
    }

    fn ended(&mut self, reason: CaptureEnd) {
        tracing::warn!(%reason, "screen capture ended by itself");
        let _ = self.events.send(BackendEvent::ScreenShareEnded { reason });
    }
}

pub(crate) struct LocalShare {
    room: Arc<Room>,
    sid: TrackSid,
    capture: Box<dyn FrameSource>,
}

pub(crate) async fn publish_share(
    room: Arc<Room>,
    events: UnboundedSender<BackendEvent>,
    mut capture: Box<dyn FrameSource>,
    target: &CaptureTarget,
    preset: SharePreset,
) -> Result<LocalShare, SessionError> {
    let video_source = Arc::new(OnceLock::new());
    let (size_tx, size_rx) = oneshot::channel();
    let sink = PublishSink {
        source: video_source.clone(),
        first_size: Some(size_tx),
        preset,
        events,
        meter: ShareMeter::new(Instant::now()),
        published: (0, 0),
    };
    capture.start(target, preset.fps(), Box::new(sink))?;

    let source_size = match timeout(FIRST_FRAME_TIMEOUT, size_rx).await {
        Ok(Ok(size)) => size,
        _ => {
            stop_capture(capture).await;
            return Err(SessionError::Video(
                "no frame was captured from the target".to_owned(),
            ));
        }
    };
    let (width, height) = fit_within(
        source_size.0,
        source_size.1,
        preset.max_width(),
        preset.max_height(),
    );
    let native = NativeVideoSource::new(VideoResolution { width, height }, true);
    let _ = video_source.set(native.clone());
    let track =
        LocalVideoTrack::create_video_track(SHARE_TRACK_NAME, RtcVideoSource::Native(native));
    let options = TrackPublishOptions {
        source: TrackSource::Screenshare,
        video_codec: VideoCodec::VP9,
        video_encoding: Some(VideoEncoding {
            max_bitrate: preset.max_bitrate_bps(),
            max_framerate: f64::from(preset.fps()),
        }),
        simulcast: false,
        ..Default::default()
    };
    let published = timeout(
        PUBLISH_TIMEOUT,
        room.local_participant()
            .publish_track(LocalTrack::Video(track), options),
    )
    .await;
    let publication = match published {
        Ok(Ok(publication)) => publication,
        Ok(Err(err)) => {
            stop_capture(capture).await;
            return Err(SessionError::Video(format!(
                "could not publish the screen share: {err}"
            )));
        }
        Err(_) => {
            stop_capture(capture).await;
            return Err(SessionError::Video(
                "publishing the screen share timed out".to_owned(),
            ));
        }
    };
    tracing::info!(
        target_name = %target.name,
        source_width = source_size.0,
        source_height = source_size.1,
        published_width = width,
        published_height = height,
        fps = preset.fps(),
        max_bitrate_bps = preset.max_bitrate_bps(),
        codec = publication.publish_options().video_codec.as_str(),
        "screen share started"
    );
    Ok(LocalShare {
        room,
        sid: publication.sid(),
        capture,
    })
}

async fn stop_capture(mut capture: Box<dyn FrameSource>) {
    if tokio::task::spawn_blocking(move || capture.stop())
        .await
        .is_err()
    {
        tracing::warn!("stopping the capture thread failed");
    }
}

pub(crate) async fn stop_share(share: LocalShare) {
    let LocalShare { room, sid, capture } = share;
    stop_capture(capture).await;
    match timeout(
        UNPUBLISH_TIMEOUT,
        room.local_participant().unpublish_track(&sid),
    )
    .await
    {
        Ok(Ok(_)) => tracing::info!("screen share stopped"),
        Ok(Err(err)) => tracing::warn!(%err, "unpublishing the screen share failed"),
        Err(_) => tracing::warn!("unpublishing the screen share timed out"),
    }
}

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
        )));
    }
}

async fn read_frames(
    track: RtcVideoTrack,
    slot: FrameSlot,
    identity: String,
    events: UnboundedSender<BackendEvent>,
) {
    let mut stream = NativeVideoStream::new(track);
    let mut meter = ShareMeter::new(Instant::now());
    let mut size = (0u32, 0u32);
    while let Some(frame) = stream.next().await {
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
