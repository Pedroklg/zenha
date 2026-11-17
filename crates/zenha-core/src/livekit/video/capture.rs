//! Captura de tela pelo `DesktopCapturer` do libwebrtc, consultado em thread própria.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use livekit::webrtc::desktop_capturer::{
    CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions, DesktopFrame,
};

use crate::api::SessionError;
use crate::backend::support::{CaptureHealth, parse_target_id, target_id, target_name};
use crate::video::capture::{
    BgraFrame, CaptureEnd, FrameSink, FrameSource, Pacer, bgra_to_video_frame,
};
use crate::video::{CaptureKind, CaptureTarget, VideoFrame};

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

    fn capture_thumbnail(
        &mut self,
        target: &CaptureTarget,
        max_side: u32,
        timeout: Duration,
    ) -> Result<VideoFrame, SessionError> {
        let (kind, source_id) = parse_target_id(&target.id)
            .ok_or_else(|| SessionError::TargetNotFound(target.id.clone()))?;
        let mut options = DesktopCapturerOptions::new(source_type(kind));
        options.set_include_cursor(false);
        let Some(mut capturer) = DesktopCapturer::new(options) else {
            return Err(SessionError::Video(
                "could not create the desktop capturer for a thumbnail".to_owned(),
            ));
        };
        let Some(source) = capturer
            .get_source_list()
            .into_iter()
            .find(|source| source.id() == source_id)
        else {
            return Err(SessionError::TargetNotFound(target.id.clone()));
        };

        let result: Arc<Mutex<Option<Result<VideoFrame, CaptureError>>>> =
            Arc::new(Mutex::new(None));
        let slot = result.clone();
        capturer.start_capture(Some(source), move |capture_result| {
            let converted = match capture_result {
                Ok(frame) => {
                    let Some(bgra) = desktop_frame_as_bgra(&frame) else {
                        *slot.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(Err(CaptureError::Temporary));
                        return;
                    };
                    // A panic must not unwind into the C++ capturer that invoked this callback.
                    match catch_unwind(AssertUnwindSafe(|| bgra_to_video_frame(&bgra, max_side))) {
                        Ok(Ok(video_frame)) => Ok(video_frame),
                        Ok(Err(_)) => Err(CaptureError::Temporary),
                        Err(_) => Err(CaptureError::Permanent),
                    }
                }
                Err(err) => Err(err),
            };
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(converted);
        });

        let deadline = Instant::now() + timeout;
        loop {
            *result.lock().unwrap_or_else(PoisonError::into_inner) = None;
            capturer.capture_frame();
            match result.lock().unwrap_or_else(PoisonError::into_inner).take() {
                Some(Ok(frame)) => return Ok(frame),
                Some(Err(CaptureError::Permanent)) => {
                    return Err(SessionError::Video(
                        "thumbnail capture failed permanently".to_owned(),
                    ));
                }
                Some(Err(CaptureError::Temporary)) | None => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(SessionError::Video(
                            "thumbnail capture timed out".to_owned(),
                        ));
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(20)));
                }
            }
        }
    }
}

fn desktop_frame_as_bgra(frame: &DesktopFrame) -> Option<BgraFrame<'_>> {
    let width = u32::try_from(frame.width()).ok()?;
    let height = u32::try_from(frame.height()).ok()?;
    let stride = usize::try_from(frame.stride()).ok()?;
    let bgra = BgraFrame {
        width,
        height,
        stride,
        data: frame.data(),
    };
    bgra.is_consistent().then_some(bgra)
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
    let Some(bgra) = desktop_frame_as_bgra(frame) else {
        state.temporary_error.store(true, Ordering::SeqCst);
        return;
    };
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
