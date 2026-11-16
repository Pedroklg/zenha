//! Screen share kept on the GPU: Windows Graphics Capture, the D3D11 video processor for
//! scaling and NV12 conversion, and a hardware H.264 encoder from Media Foundation.
#![expect(
    unsafe_code,
    reason = "Direct3D 11, Windows Graphics Capture and Media Foundation are COM APIs without safe wrappers"
)]

mod capture;
mod convert;
mod encode;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use livekit::webrtc::video_frame::{EncodedFrameType, EncodedVideoCodec, EncodedVideoFrame};
use livekit::webrtc::video_source::VideoResolution;
use livekit::webrtc::video_source::native::NativeVideoSource;
use tokio::sync::oneshot;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Media::MediaFoundation::{MFSTARTUP_LITE, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::Interface;

use crate::api::SessionError;
use crate::capture::{FrameSource, Pacer};
use crate::video::{CaptureTarget, VideoFrame, fit_within};

const MF_VERSION: u32 = 0x0002_0070;
const KEEPALIVE: Duration = Duration::from_secs(1);
const STATS_WINDOW: Duration = Duration::from_secs(5);
const SOURCE_WAIT_STEP: Duration = Duration::from_millis(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum GpuError {
    #[error("{step}: {source}")]
    Windows {
        step: &'static str,
        source: windows::core::Error,
    },
    #[error("{0}")]
    Unsupported(&'static str),
}

trait Step<T> {
    fn step(self, step: &'static str) -> Result<T, GpuError>;
}

impl<T> Step<T> for windows::core::Result<T> {
    fn step(self, step: &'static str) -> Result<T, GpuError> {
        self.map_err(|source| GpuError::Windows { step, source })
    }
}

pub(crate) fn enabled() -> bool {
    std::env::var_os("ZENHA_GPU_SHARE").is_some_and(|value| value == "1")
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GpuShareConfig {
    pub device_index: u32,
    pub max_size: (u32, u32),
    pub fps: u32,
    pub bitrate_bps: u64,
}

pub(crate) type SizeReport = Result<(u32, u32), String>;

/// Capture, conversion and encoding thread; stopping it ends the share.
pub(crate) struct GpuShare {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GpuShare {
    pub(crate) fn start(
        config: GpuShareConfig,
        source: Arc<OnceLock<NativeVideoSource>>,
        size_tx: oneshot::Sender<SizeReport>,
    ) -> Result<Self, SessionError> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("gpu-share".to_owned())
            .spawn(move || run(config, &flag, &source, size_tx))
            .map_err(|err| SessionError::Video(format!("could not start the GPU share: {err}")))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("gpu share thread panicked");
        }
    }
}

impl Drop for GpuShare {
    fn drop(&mut self) {
        self.halt();
    }
}

impl FrameSource for GpuShare {
    fn list_targets(&mut self) -> Result<Vec<CaptureTarget>, SessionError> {
        Ok(Vec::new())
    }

    fn start(
        &mut self,
        _target: &CaptureTarget,
        _fps: u32,
        _sink: Box<dyn crate::capture::FrameSink>,
    ) -> Result<(), SessionError> {
        Err(SessionError::Video(
            "the GPU share starts on its own".to_owned(),
        ))
    }

    fn stop(&mut self) {
        self.halt();
    }

    fn capture_thumbnail(
        &mut self,
        _target: &CaptureTarget,
        _max_side: u32,
        _timeout: Duration,
    ) -> Result<VideoFrame, SessionError> {
        Err(SessionError::Video(
            "the GPU share has no thumbnails".to_owned(),
        ))
    }
}

struct Runtime;

impl Runtime {
    fn enter() -> Result<Self, GpuError> {
        // SAFETY: called once at the start of this thread, balanced by `CoUninitialize` in `Drop`.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .step("CoInitializeEx")?;
        // SAFETY: Media Foundation startup with the SDK version, balanced by `MFShutdown` in `Drop`.
        if let Err(source) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) } {
            // SAFETY: balances the successful `CoInitializeEx` above on this thread.
            unsafe { CoUninitialize() };
            return Err(GpuError::Windows {
                step: "MFStartup",
                source,
            });
        }
        Ok(Self)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: balances `MFStartup` and `CoInitializeEx` from `enter` on the same thread.
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), GpuError> {
    let mut device = None;
    let mut context = None;
    // SAFETY: the out pointers refer to live locals; no adapter means the default hardware one.
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .step("D3D11CreateDevice")?;
    let (Some(device), Some(context)) = (device, context) else {
        return Err(GpuError::Unsupported(
            "D3D11CreateDevice returned no device",
        ));
    };
    let multithread: ID3D11Multithread = context.cast().step("ID3D11Multithread")?;
    // SAFETY: plain setter on a live interface; the encoder uses the device from its own threads.
    let _ = unsafe { multithread.SetMultithreadProtected(true) };
    Ok((device, context))
}

struct Pipeline {
    context: ID3D11DeviceContext,
    capture: capture::ScreenCapture,
    converter: convert::Converter,
    encoder: encode::Encoder,
    out_size: (u32, u32),
    _device: ID3D11Device,
}

impl Pipeline {
    fn new(config: &GpuShareConfig) -> Result<Self, GpuError> {
        let (device, context) = create_device()?;
        let monitor = capture::monitor_for_device_index(config.device_index)?;
        let capture = capture::ScreenCapture::new(&device, monitor, config.fps)?;
        let (src_w, src_h) = capture.size();
        let out_size = fit_within(src_w, src_h, config.max_size.0, config.max_size.1);
        let out_size = (out_size.0 & !1, out_size.1 & !1);
        let converter =
            convert::Converter::new(&device, &context, (src_w, src_h), out_size, config.fps)?;
        let encoder = encode::Encoder::new(&device, out_size, config.fps, config.bitrate_bps)?;
        tracing::info!(
            source_width = src_w,
            source_height = src_h,
            published_width = out_size.0,
            published_height = out_size.1,
            encoder = %encoder.name(),
            "gpu share pipeline ready"
        );
        Ok(Self {
            context,
            capture,
            converter,
            encoder,
            out_size,
            _device: device,
        })
    }
}

#[derive(Default)]
struct Stats {
    captured: u64,
    submitted: u64,
    skipped: u64,
    encoded: u64,
    keyframes: u64,
    bytes: u64,
    busy: Duration,
}

impl Stats {
    fn log(&mut self, window: Duration) {
        let secs = window.as_secs_f64().max(0.001);
        let busy_ms = self.busy.as_secs_f64() * 1000.0 / self.submitted.max(1) as f64;
        tracing::info!(
            captured = self.captured,
            submitted = self.submitted,
            skipped = self.skipped,
            encoded = self.encoded,
            keyframes = self.keyframes,
            kbps = (self.bytes as f64 * 8.0 / secs / 1000.0).round(),
            avg_submit_ms = (busy_ms * 100.0).round() / 100.0,
            window_secs = (secs * 10.0).round() / 10.0,
            "gpu share stats"
        );
        *self = Self::default();
    }
}

fn run(
    config: GpuShareConfig,
    stop: &AtomicBool,
    slot: &OnceLock<NativeVideoSource>,
    size_tx: oneshot::Sender<SizeReport>,
) {
    let _runtime = match Runtime::enter() {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = size_tx.send(Err(err.to_string()));
            return;
        }
    };
    let mut pipeline = match Pipeline::new(&config) {
        Ok(pipeline) => pipeline,
        Err(err) => {
            let _ = size_tx.send(Err(err.to_string()));
            return;
        }
    };
    if size_tx.send(Ok(pipeline.out_size)).is_err() {
        return;
    }
    let source = loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if let Some(source) = slot.get() {
            break source.clone();
        }
        std::thread::sleep(SOURCE_WAIT_STEP);
    };
    if let Err(err) = encode_loop(&config, stop, &mut pipeline, &source) {
        tracing::warn!(%err, "gpu share stopped");
    }
}

fn encode_loop(
    config: &GpuShareConfig,
    stop: &AtomicBool,
    pipeline: &mut Pipeline,
    source: &NativeVideoSource,
) -> Result<(), GpuError> {
    let start = Instant::now();
    let mut pacer = Pacer::new(config.fps, start);
    let mut stats = Stats::default();
    let mut window_start = start;
    let mut last_submit: Option<Instant> = None;
    let mut force_key = true;
    let (width, height) = pipeline.out_size;
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(pacer.until_next(Instant::now()));
        force_key |= source.take_keyframe_request();
        if let Some(rate) = source.take_rate_control_request() {
            pipeline
                .encoder
                .set_bitrate(rate.target_bitrate_bps.min(config.bitrate_bps));
        }
        let fresh = pipeline
            .capture
            .copy_latest(&pipeline.context, pipeline.converter.input())?;
        stats.captured += u64::from(fresh);
        let due = last_submit.is_none_or(|at| at.elapsed() >= KEEPALIVE);
        let began = Instant::now();
        let outputs = if fresh || force_key || due {
            let texture = pipeline.converter.convert()?;
            let timestamp = i64::try_from(start.elapsed().as_nanos() / 100).unwrap_or(i64::MAX);
            let (outputs, accepted) = pipeline.encoder.encode(&texture, timestamp, force_key)?;
            if accepted {
                stats.submitted += 1;
                stats.busy += began.elapsed();
                force_key = false;
                last_submit = Some(Instant::now());
            } else {
                stats.skipped += 1;
            }
            outputs
        } else {
            pipeline.encoder.poll()?
        };
        for output in outputs {
            stats.encoded += 1;
            stats.keyframes += u64::from(output.keyframe);
            stats.bytes += output.data.len() as u64;
            source.capture_encoded_frame(&EncodedVideoFrame {
                codec: EncodedVideoCodec::H264,
                payload: &output.data,
                timestamp_us: 0,
                frame_type: if output.keyframe {
                    EncodedFrameType::Key
                } else {
                    EncodedFrameType::Delta
                },
                resolution: VideoResolution { width, height },
                frame_metadata: None,
            });
        }
        let now = Instant::now();
        pacer.finish_slot(now);
        if now.duration_since(window_start) >= STATS_WINDOW {
            stats.log(now.duration_since(window_start));
            window_start = now;
        }
    }
    Ok(())
}
