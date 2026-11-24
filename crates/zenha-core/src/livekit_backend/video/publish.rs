//! Publishing and ending the screen share as a local video track.

use std::sync::{Arc, Mutex as StdMutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoding};
use livekit::prelude::{LocalTrack, LocalVideoTrack, Room, TrackSid, TrackSource};
use livekit::webrtc::rtp_sender::VideoEncoderBackend;
use livekit::webrtc::video_frame::{I420Buffer, VideoFrame as RtcFrame, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::{RtcVideoSource, VideoResolution};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::convert::BgraToI420Converter;
use crate::api::SessionError;
use crate::backend::BackendEvent;
use crate::backend::support::{ShareMeter, parse_target_id};
use crate::video::capture::{BgraFrame, CaptureEnd, FrameSink, FrameSource, bgra_to_rgba_scaled};
use crate::video::{
    CaptureTarget, FrameSlot, PreviewGate, SHARE_PREVIEW_INTERVAL, SHARE_PREVIEW_MAX_SIDE,
    SharePreset, fit_within, gpu_share,
};

const SHARE_TRACK_NAME: &str = "screen";
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(15);
const UNPUBLISH_TIMEOUT: Duration = Duration::from_secs(5);
const DYNACAST_POLL: Duration = Duration::from_millis(250);

/// Copies the e2e test-pattern frames to the P2P senders, which encode them in `libwebrtc`.
#[derive(Default)]
pub(crate) struct RawVideoFanOut {
    sinks: StdMutex<Vec<(String, NativeVideoSource)>>,
}

impl RawVideoFanOut {
    pub(crate) fn add(&self, id: impl Into<String>, source: NativeVideoSource) {
        let id = id.into();
        let mut sinks = self.sinks.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, existing)) = sinks.iter_mut().find(|(sink_id, _)| *sink_id == id) {
            *existing = source;
        } else {
            sinks.push((id, source));
        }
    }

    pub(crate) fn remove(&self, id: &str) {
        self.sinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(sink_id, _)| sink_id != id);
    }

    fn send_frame(&self, frame: &RtcFrame<I420Buffer>) {
        let sinks = self.sinks.lock().unwrap_or_else(PoisonError::into_inner);
        for (_, source) in sinks.iter() {
            source.capture_frame(frame);
        }
    }
}

pub(crate) struct PublishSink {
    source: Arc<OnceLock<NativeVideoSource>>,
    raw_fan_out: Option<Arc<RawVideoFanOut>>,
    first_size: Option<oneshot::Sender<(u32, u32)>>,
    preset: SharePreset,
    events: UnboundedSender<BackendEvent>,
    meter: ShareMeter,
    published: (u32, u32),
    preview: FrameSlot,
    preview_gate: PreviewGate,
    ui_visible: Arc<std::sync::atomic::AtomicBool>,
    converter: BgraToI420Converter,
}

impl PublishSink {
    fn push_preview(&mut self, frame: &BgraFrame<'_>, now: Instant) {
        if !self.ui_visible.load(std::sync::atomic::Ordering::Relaxed)
            || !self.preview_gate.take(now)
        {
            return;
        }
        let (pw, ph) = fit_within(
            frame.width,
            frame.height,
            SHARE_PREVIEW_MAX_SIDE,
            SHARE_PREVIEW_MAX_SIDE,
        );
        self.preview
            .push_with(pw, ph, |dst| bgra_to_rgba_scaled(frame, pw, ph, dst));
    }
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
        let Some(buffer) = self.converter.convert(&frame, out) else {
            return;
        };
        let rtc_frame = RtcFrame::new(VideoRotation::VideoRotation0, buffer);
        source.capture_frame(&rtc_frame);
        if let Some(raw_fan_out) = &self.raw_fan_out {
            raw_fan_out.send_frame(&rtc_frame);
        }
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
        self.push_preview(&frame, now);
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
    sid: Arc<std::sync::Mutex<TrackSid>>,
    capture: Box<dyn FrameSource>,
    fan_out: Option<Arc<gpu_share::VideoFanOut>>,
    raw_fan_out: Option<Arc<RawVideoFanOut>>,
    preset: SharePreset,
    resolution: (u32, u32),
}

impl LocalShare {
    pub(crate) fn sid_handle(&self) -> Arc<std::sync::Mutex<TrackSid>> {
        Arc::clone(&self.sid)
    }

    pub(crate) fn fan_out(&self) -> Option<Arc<gpu_share::VideoFanOut>> {
        self.fan_out.clone()
    }

    pub(crate) fn raw_fan_out(&self) -> Option<Arc<RawVideoFanOut>> {
        self.raw_fan_out.clone()
    }

    pub(crate) fn preset(&self) -> SharePreset {
        self.preset
    }

    pub(crate) fn resolution(&self) -> (u32, u32) {
        self.resolution
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ShareOptions {
    pub(crate) preset: SharePreset,
    pub(crate) video_codec: crate::backend::options::VideoCodec,
    /// Test pattern only: keep a [`RawVideoFanOut`] so P2P works without the GPU encoder.
    pub(crate) p2p_raw_test_source: bool,
}

fn livekit_video_codec(codec: crate::backend::options::VideoCodec) -> VideoCodec {
    match codec {
        crate::backend::options::VideoCodec::Vp9 => VideoCodec::VP9,
        crate::backend::options::VideoCodec::H264 => VideoCodec::H264,
        crate::backend::options::VideoCodec::Av1 => VideoCodec::AV1,
    }
}

pub(crate) async fn publish_share(
    room: Arc<Room>,
    events: UnboundedSender<BackendEvent>,
    mut capture: Box<dyn FrameSource>,
    target: &CaptureTarget,
    share_options: ShareOptions,
    preview: FrameSlot,
    ui_visible: Arc<std::sync::atomic::AtomicBool>,
) -> Result<LocalShare, SessionError> {
    let ShareOptions {
        preset,
        video_codec,
        p2p_raw_test_source,
    } = share_options;
    if gpu_share::enabled()
        && let Some(gpu_target) = parse_target_id(&target.id)
            .and_then(|(kind, source_id)| gpu_share::GpuTarget::new(kind, source_id))
    {
        let fan_out = Arc::new(gpu_share::VideoFanOut::default());
        let output = gpu_share::ShareOutput {
            fan_out: Arc::clone(&fan_out),
            preview: preview.clone(),
            ui_visible: Arc::clone(&ui_visible),
            on_end: Box::new({
                let events = events.clone();
                move |reason| {
                    let _ = events.send(BackendEvent::ScreenShareEnded { reason });
                }
            }),
        };
        match publish_gpu_share(room.clone(), target, preset, gpu_target, output).await {
            Ok(share) => return Ok(share),
            Err(err) => {
                tracing::warn!(%err, "gpu screen share unavailable, using the default path")
            }
        }
    }
    let video_source = Arc::new(OnceLock::new());
    let raw_fan_out = p2p_raw_test_source.then(|| Arc::new(RawVideoFanOut::default()));
    let (size_tx, size_rx) = oneshot::channel();
    let sink = PublishSink {
        source: video_source.clone(),
        raw_fan_out: raw_fan_out.clone(),
        first_size: Some(size_tx),
        preset,
        events,
        meter: ShareMeter::new(Instant::now()),
        published: (0, 0),
        preview,
        preview_gate: PreviewGate::new(SHARE_PREVIEW_INTERVAL),
        ui_visible,
        converter: BgraToI420Converter::default(),
    };
    capture.start(target, preset.fps(), Box::new(sink))?;

    let Ok(Ok(source_size)) = timeout(FIRST_FRAME_TIMEOUT, size_rx).await else {
        stop_capture(capture).await;
        return Err(SessionError::Video(
            "no frame was captured from the target".to_owned(),
        ));
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
        video_codec: livekit_video_codec(video_codec),
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
        sid: Arc::new(std::sync::Mutex::new(publication.sid())),
        capture,
        fan_out: None,
        raw_fan_out,
        preset,
        resolution: (width, height),
    })
}

async fn publish_gpu_share(
    room: Arc<Room>,
    target: &CaptureTarget,
    preset: SharePreset,
    gpu_target: gpu_share::GpuTarget,
    output: gpu_share::ShareOutput,
) -> Result<LocalShare, SessionError> {
    let fan_out = Arc::clone(&output.fan_out);
    let (size_tx, size_rx) = oneshot::channel();
    let config = gpu_share::GpuShareConfig {
        target: gpu_target,
        max_size: (preset.max_width(), preset.max_height()),
        fps: preset.fps(),
        bitrate_bps: preset.max_bitrate_bps(),
    };
    let share = gpu_share::GpuShare::start(config, output, size_tx)?;
    let capture: Box<dyn FrameSource> = Box::new(share);
    let (width, height) = match timeout(FIRST_FRAME_TIMEOUT, size_rx).await {
        Ok(Ok(Ok(size))) => size,
        Ok(Ok(Err(err))) => {
            stop_capture(capture).await;
            return Err(SessionError::Video(err));
        }
        _ => {
            stop_capture(capture).await;
            return Err(SessionError::Video(
                "the GPU share did not start in time".to_owned(),
            ));
        }
    };
    let native = NativeVideoSource::new_encoded(VideoResolution { width, height });
    fan_out.add(gpu_share::SFU_SINK_ID, native.clone());
    let track =
        LocalVideoTrack::create_video_track(SHARE_TRACK_NAME, RtcVideoSource::Native(native));
    let dynacast_track = track.clone();
    let options = TrackPublishOptions {
        source: TrackSource::Screenshare,
        video_codec: VideoCodec::H264,
        video_encoding: Some(VideoEncoding {
            max_bitrate: preset.max_bitrate_bps(),
            max_framerate: f64::from(preset.fps()),
        }),
        simulcast: false,
        video_encoder: VideoEncoderBackend::PreEncoded,
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
            fan_out.remove(gpu_share::SFU_SINK_ID);
            stop_capture(capture).await;
            return Err(SessionError::Video(format!(
                "could not publish the GPU screen share: {err}"
            )));
        }
        Err(_) => {
            fan_out.remove(gpu_share::SFU_SINK_ID);
            stop_capture(capture).await;
            return Err(SessionError::Video(
                "publishing the GPU screen share timed out".to_owned(),
            ));
        }
    };
    tracing::info!(
        target_name = %target.name,
        published_width = width,
        published_height = height,
        fps = preset.fps(),
        max_bitrate_bps = preset.max_bitrate_bps(),
        codec = publication.publish_options().video_codec.as_str(),
        "gpu screen share started"
    );
    follow_dynacast(dynacast_track, Arc::clone(&fan_out));
    Ok(LocalShare {
        room,
        sid: Arc::new(std::sync::Mutex::new(publication.sid())),
        capture,
        fan_out: Some(fan_out),
        raw_fan_out: None,
        preset,
        resolution: (width, height),
    })
}

/// The pass-through encoder never sees frames while dynacast pauses the layer, so it cannot
/// report the pause; mirror the sender's layer state into the SFU sink instead.
fn follow_dynacast(track: LocalVideoTrack, fan_out: Arc<gpu_share::VideoFanOut>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(DYNACAST_POLL);
        let mut last = true;
        loop {
            tick.tick().await;
            let layers = track.publishing_layers();
            let active = layers.is_empty() || layers.iter().any(|layer| layer.active);
            if !fan_out.set_active(gpu_share::SFU_SINK_ID, active) {
                break;
            }
            if active != last {
                tracing::debug!(active, "sfu screen share layer changed");
                last = active;
            }
        }
    });
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
    let LocalShare {
        room,
        sid,
        capture,
        fan_out,
        ..
    } = share;
    if let Some(fan_out) = fan_out {
        fan_out.remove(gpu_share::SFU_SINK_ID);
    }
    let sid = sid
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
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
