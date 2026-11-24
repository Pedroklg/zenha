//! Screen share kept on the GPU: Windows Graphics Capture, the D3D11 video processor for
//! scaling and color conversion, and a hardware H.264 encoder from Media Foundation.
#![expect(
    unsafe_code,
    reason = "Direct3D 11, Windows Graphics Capture and Media Foundation are COM APIs without safe wrappers"
)]

mod capture;
mod convert;
mod encode;
mod fan_out;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use livekit::webrtc::video_frame::{EncodedFrameType, EncodedVideoCodec, EncodedVideoFrame};
use livekit::webrtc::video_source::VideoResolution;
use tokio::sync::oneshot;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Media::MediaFoundation::{MFSTARTUP_LITE, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::core::{Interface, PCWSTR};

use crate::api::SessionError;
use crate::video::capture::{CaptureEnd, FrameSource, Pacer};
use crate::video::{
    CaptureKind, CaptureTarget, FrameSlot, PreviewGate, SHARE_PREVIEW_INTERVAL,
    SHARE_PREVIEW_MAX_SIDE, VideoFrame, fit_within,
};

pub(crate) use fan_out::{SFU_SINK_ID, VideoFanOut};

const MF_VERSION: u32 = 0x0002_0070;
const KEEPALIVE: Duration = Duration::from_secs(1);
const STATS_WINDOW: Duration = Duration::from_secs(5);
const SOURCE_WAIT_STEP: Duration = Duration::from_millis(10);
const RESIZE_SETTLE: Duration = Duration::from_millis(250);
const MIN_SIDE: u32 = 64;
const ENCODER_RING: usize = 4;
const LATENCY_CAPACITY: usize = 320;
const MAX_IN_FLIGHT_TIMESTAMPS: usize = 16;

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

/// On unless `ZENHA_GPU_SHARE=0`.
pub(crate) fn enabled() -> bool {
    std::env::var_os("ZENHA_GPU_SHARE").is_none_or(|value| value != "0")
}

/// GPU encoder backend requested via `ZENHA_GPU_ENCODER`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum GpuEncoderPreference {
    #[default]
    Auto,
    Mft,
    Amf,
    Nvenc,
}

impl GpuEncoderPreference {
    /// Parses a `ZENHA_GPU_ENCODER` value (trimmed, ASCII case-insensitive).
    /// Empty strings select `Auto`; unknown values return `None`.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Some(Self::Auto),
            "mft" | "mf" => Some(Self::Mft),
            "amf" => Some(Self::Amf),
            "nvenc" => Some(Self::Nvenc),
            _ => None,
        }
    }

    pub(crate) fn from_env() -> Self {
        let Ok(raw) = std::env::var("ZENHA_GPU_ENCODER") else {
            return Self::Auto;
        };
        match Self::parse(&raw) {
            Some(choice) => choice,
            None => {
                tracing::warn!(
                    value = %raw,
                    "unknown ZENHA_GPU_ENCODER value, falling back to auto"
                );
                Self::Auto
            }
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Mft => "mft",
            Self::Amf => "amf",
            Self::Nvenc => "nvenc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GpuTarget {
    /// libwebrtc screen id: the `EnumDisplayDevices` index.
    Monitor(u32),
    /// libwebrtc window id: the `HWND`.
    Window(isize),
}

impl GpuTarget {
    pub(crate) fn new(kind: CaptureKind, source_id: u64) -> Option<Self> {
        match kind {
            CaptureKind::Screen => u32::try_from(source_id).ok().map(Self::Monitor),
            CaptureKind::Window => isize::try_from(source_id).ok().map(Self::Window),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GpuShareConfig {
    pub target: GpuTarget,
    pub max_size: (u32, u32),
    pub fps: u32,
    pub bitrate_bps: u64,
}

/// Where the share thread delivers its encoded frames and local preview.
pub(crate) struct ShareOutput {
    pub fan_out: Arc<VideoFanOut>,
    pub preview: FrameSlot,
    pub ui_visible: Arc<AtomicBool>,
    pub on_end: Box<dyn FnOnce(CaptureEnd) + Send>,
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
        output: ShareOutput,
        size_tx: oneshot::Sender<SizeReport>,
    ) -> Result<Self, SessionError> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("gpu-share".to_owned())
            .spawn(move || run(config, &flag, output, size_tx))
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
        _sink: Box<dyn crate::video::capture::FrameSink>,
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

struct HighResTimer(Option<HANDLE>);

impl HighResTimer {
    fn new() -> Self {
        // SAFETY: unnamed high-resolution waitable timer without security attributes; closed in `Drop`.
        let handle = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .ok();
        Self(handle)
    }

    fn sleep(&self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        let Some(handle) = self.0 else {
            std::thread::sleep(duration);
            return;
        };
        let ticks = i64::try_from((duration.as_nanos() / 100).max(1)).unwrap_or(i64::MAX);
        let due_time = -ticks;
        // SAFETY: `handle` is a valid waitable timer owned by `self` and `due_time` is a live local.
        let armed = unsafe { SetWaitableTimer(handle, &due_time, 0, None, None, false) }.is_ok();
        if armed {
            // SAFETY: `handle` is a valid armed waitable timer; waiting blocks until it signals.
            unsafe {
                let _ = WaitForSingleObject(handle, INFINITE);
            }
        } else {
            std::thread::sleep(duration);
        }
    }
}

impl Drop for HighResTimer {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // SAFETY: `handle` was returned by `CreateWaitableTimerExW` and is closed once here.
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
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

/// Published size for a source: within the preset, even, and not degenerate.
fn output_size(source: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (width, height) = fit_within(source.0, source.1, max.0, max.1);
    ((width & !1).max(MIN_SIDE), (height & !1).max(MIN_SIDE))
}

/// Waits for a new capture size to settle before the pipeline is rebuilt for it.
#[derive(Default)]
struct ResizeWatch {
    pending: Option<((u32, u32), Instant)>,
}

impl ResizeWatch {
    fn observe(&mut self, current: (u32, u32), content: (u32, u32), now: Instant) {
        if content == current || content.0 < 2 || content.1 < 2 {
            self.pending = None;
        } else if self.pending.is_none_or(|(size, _)| size != content) {
            self.pending = Some((content, now));
        }
    }

    fn due(&mut self, now: Instant) -> bool {
        let due = self
            .pending
            .is_some_and(|(_, since)| now.saturating_duration_since(since) >= RESIZE_SETTLE);
        if due {
            self.pending = None;
        }
        due
    }
}

struct Pipeline {
    capture: capture::ScreenCapture,
    input: ID3D11Texture2D,
    converter: convert::Converter,
    encoder: encode::Encoder,
    preview: convert::Preview,
    out_size: (u32, u32),
}

impl Pipeline {
    fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: &GpuShareConfig,
    ) -> Result<Self, GpuError> {
        let requested_encoder = GpuEncoderPreference::from_env();
        if matches!(
            requested_encoder,
            GpuEncoderPreference::Amf | GpuEncoderPreference::Nvenc
        ) {
            tracing::warn!(
                requested = requested_encoder.as_str(),
                "requested GPU encoder backend is not available yet, using Media Foundation MFT"
            );
        }
        let capture = capture::ScreenCapture::new(device, config.target, config.fps)?;
        let source = capture.size();
        let input = convert::texture(
            device,
            source,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        )?;
        let out_size = output_size(source, config.max_size);
        let converter = convert::Converter::new(
            device,
            context,
            &input,
            (source, out_size),
            config.fps,
            (convert::Output::Nv12, ENCODER_RING),
        )?;
        let encoder = encode::Encoder::new(device, out_size, config.fps, config.bitrate_bps)?;
        let preview_size = fit_within(
            source.0,
            source.1,
            SHARE_PREVIEW_MAX_SIDE,
            SHARE_PREVIEW_MAX_SIDE,
        );
        let preview = convert::Preview::new(device, context, &input, source, preview_size)?;
        tracing::info!(
            source_width = source.0,
            source_height = source.1,
            published_width = out_size.0,
            published_height = out_size.1,
            encoder = %encoder.name(),
            codec = encoder.codec_label(),
            requested_encoder = requested_encoder.as_str(),
            "gpu share pipeline ready"
        );
        Ok(Self {
            capture,
            input,
            converter,
            encoder,
            preview,
            out_size,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LatencyPercentiles {
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
}

/// Reusable sample buffer for nearest-rank latency percentiles across a stats window.
struct LatencyRecorder {
    samples_us: Vec<u64>,
}

impl Default for LatencyRecorder {
    fn default() -> Self {
        Self {
            samples_us: Vec::with_capacity(LATENCY_CAPACITY),
        }
    }
}

impl LatencyRecorder {
    fn record(&mut self, duration: Duration) {
        self.samples_us
            .push(u64::try_from(duration.as_micros()).unwrap_or(u64::MAX));
    }

    fn percentiles(&mut self) -> LatencyPercentiles {
        if self.samples_us.is_empty() {
            return LatencyPercentiles {
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
            };
        }
        self.samples_us.sort_unstable();
        LatencyPercentiles {
            p50_ms: self.rank_ms(50),
            p95_ms: self.rank_ms(95),
            p99_ms: self.rank_ms(99),
        }
    }

    fn rank_ms(&self, pct: usize) -> f64 {
        let len = self.samples_us.len();
        let idx = (len.saturating_mul(pct))
            .div_ceil(100)
            .saturating_sub(1)
            .min(len - 1);
        self.samples_us[idx] as f64 / 1000.0
    }

    fn clear(&mut self) {
        self.samples_us.clear();
    }
}

/// Correlates submitted frame timestamps with completed encoded outputs to measure real encode time.
#[derive(Default)]
struct InFlightTimestamps {
    entries: VecDeque<(i64, Instant)>,
}

impl InFlightTimestamps {
    fn push(&mut self, time_us: i64, submitted_at: Instant) {
        if self.entries.len() >= MAX_IN_FLIGHT_TIMESTAMPS {
            self.entries.pop_front();
        }
        self.entries.push_back((time_us, submitted_at));
    }

    fn take(&mut self, output_time_us: i64) -> Option<Instant> {
        if let Some(pos) = self.entries.iter().position(|&(t, _)| t == output_time_us) {
            for _ in 0..pos {
                self.entries.pop_front();
            }
            return self.entries.pop_front().map(|(_, at)| at);
        }
        self.entries.pop_front().map(|(_, at)| at)
    }

    fn clear(&mut self) {
        self.entries.clear();
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
    target_bitrate_bps: u64,
    active_sinks: usize,
    busy: Duration,
    last_output: Option<Instant>,
    max_gap: Duration,
    encode_latency: LatencyRecorder,
}

impl Stats {
    fn log(&mut self, window: Duration, encoder: &str, codec: &str) {
        let secs = window.as_secs_f64().max(0.001);
        let busy_ms = self.busy.as_secs_f64() * 1000.0 / self.submitted.max(1) as f64;
        let pct = self.encode_latency.percentiles();
        tracing::info!(
            encoder,
            codec,
            captured = self.captured,
            submitted = self.submitted,
            skipped = self.skipped,
            encoded = self.encoded,
            keyframes = self.keyframes,
            kbps = (self.bytes as f64 * 8.0 / secs / 1000.0).round(),
            target_kbps = (self.target_bitrate_bps as f64 / 1000.0).round(),
            active_sinks = self.active_sinks,
            avg_submit_ms = format_args!("{busy_ms:.2}"),
            encode_p50_ms = format_args!("{:.2}", pct.p50_ms),
            encode_p95_ms = format_args!("{:.2}", pct.p95_ms),
            encode_p99_ms = format_args!("{:.2}", pct.p99_ms),
            max_gap_ms = self.max_gap.as_millis(),
            window_secs = format_args!("{secs:.1}"),
            "gpu share stats"
        );
        self.reset();
    }

    fn reset(&mut self) {
        self.captured = 0;
        self.submitted = 0;
        self.skipped = 0;
        self.encoded = 0;
        self.keyframes = 0;
        self.bytes = 0;
        self.busy = Duration::ZERO;
        self.last_output = None;
        self.max_gap = Duration::ZERO;
        self.encode_latency.clear();
    }
}

fn run(
    config: GpuShareConfig,
    stop: &AtomicBool,
    output: ShareOutput,
    size_tx: oneshot::Sender<SizeReport>,
) {
    let _runtime = match Runtime::enter() {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = size_tx.send(Err(err.to_string()));
            return;
        }
    };
    let started = create_device().and_then(|(device, context)| {
        let pipeline = Pipeline::new(&device, &context, &config)?;
        Ok((device, context, pipeline))
    });
    let (device, context, pipeline) = match started {
        Ok(started) => started,
        Err(err) => {
            let _ = size_tx.send(Err(err.to_string()));
            return;
        }
    };
    if size_tx.send(Ok(pipeline.out_size)).is_err() {
        return;
    }
    while !output.fan_out.has_active_sinks() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(SOURCE_WAIT_STEP);
    }
    let mut share = Share {
        config,
        device,
        context,
        pipeline: Some(pipeline),
        fan_out: output.fan_out,
        preview: output.preview,
        preview_gate: PreviewGate::new(SHARE_PREVIEW_INTERVAL),
        ui_visible: output.ui_visible,
    };
    let end = match share.run(stop) {
        Ok(None) => return,
        Ok(Some(end)) => end,
        Err(err) => CaptureEnd::Failed(err.to_string()),
    };
    if !stop.load(Ordering::SeqCst) {
        tracing::warn!(reason = %end, "gpu share ended by itself");
        (output.on_end)(end);
    }
}

struct Share {
    config: GpuShareConfig,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    pipeline: Option<Pipeline>,
    fan_out: Arc<VideoFanOut>,
    preview: FrameSlot,
    preview_gate: PreviewGate,
    ui_visible: Arc<AtomicBool>,
}

impl Share {
    /// Runs until stopped (`None`) or until the target goes away.
    fn run(&mut self, stop: &AtomicBool) -> Result<Option<CaptureEnd>, GpuError> {
        let start = Instant::now();
        let mut pacer = Pacer::new(self.config.fps, start);
        let timer = HighResTimer::new();
        let mut stats = Stats {
            target_bitrate_bps: self.config.bitrate_bps,
            ..Stats::default()
        };
        let mut in_flight = InFlightTimestamps::default();
        let mut window_start = start;
        let mut last_submit: Option<Instant> = None;
        let mut force_key = true;
        let mut resize = ResizeWatch::default();
        while !stop.load(Ordering::SeqCst) {
            timer.sleep(pacer.until_next(Instant::now()));
            let Some(pipeline) = self.pipeline.as_mut() else {
                return Err(GpuError::Unsupported("gpu share pipeline missing"));
            };
            if pipeline.capture.closed() {
                return Ok(Some(CaptureEnd::TargetClosed));
            }
            let controls = self.fan_out.poll_controls(self.config.bitrate_bps);
            force_key |= controls.force_keyframe;
            stats.active_sinks = controls.active_sinks;
            if let Some(target_bps) = controls.target_bitrate_bps {
                stats.target_bitrate_bps = target_bps;
                pipeline.encoder.set_bitrate(target_bps);
            }
            let captured = pipeline
                .capture
                .copy_latest(&self.context, &pipeline.input)?;
            let now = Instant::now();
            if let Some(content) = captured {
                stats.captured += 1;
                resize.observe(pipeline.capture.size(), content, now);
            }
            if resize.due(now) {
                self.pipeline = None;
                self.pipeline = Some(Pipeline::new(&self.device, &self.context, &self.config)?);
                force_key = true;
                last_submit = None;
                in_flight.clear();
                continue;
            }
            let fresh = captured.is_some();
            if fresh {
                self.push_preview(now);
            }
            let Some(pipeline) = self.pipeline.as_mut() else {
                return Err(GpuError::Unsupported("gpu share pipeline missing"));
            };
            let due = last_submit.is_none_or(|at| at.elapsed() >= KEEPALIVE);
            let began = Instant::now();
            let outputs = if controls.active_sinks == 0 {
                force_key = true;
                pipeline.encoder.poll()?
            } else if fresh || force_key || due {
                let texture = pipeline.converter.convert()?;
                let timestamp = i64::try_from(start.elapsed().as_nanos() / 100).unwrap_or(i64::MAX);
                let encode_start = Instant::now();
                let (outputs, accepted) =
                    pipeline.encoder.encode(&texture, timestamp, force_key)?;
                if accepted {
                    in_flight.push(timestamp / 10, encode_start);
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
            let (width, height) = pipeline.out_size;
            for output in outputs {
                stats.encoded += 1;
                let sent = Instant::now();
                if let Some(submitted_at) = in_flight.take(output.time_us) {
                    stats
                        .encode_latency
                        .record(sent.saturating_duration_since(submitted_at));
                }
                if let Some(previous) = stats.last_output.replace(sent) {
                    stats.max_gap = stats.max_gap.max(sent - previous);
                }
                stats.keyframes += u64::from(output.keyframe);
                stats.bytes += output.data.len() as u64;
                self.fan_out.send_frame(&EncodedVideoFrame {
                    codec: EncodedVideoCodec::H264,
                    payload: &output.data,
                    timestamp_us: output.time_us,
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
                stats.log(
                    now.duration_since(window_start),
                    pipeline.encoder.name(),
                    pipeline.encoder.codec_label(),
                );
                window_start = now;
            }
        }
        Ok(None)
    }

    fn push_preview(&mut self, now: Instant) {
        if !self.ui_visible.load(Ordering::Relaxed) || !self.preview_gate.take(now) {
            return;
        }
        let Some(pipeline) = self.pipeline.as_mut() else {
            return;
        };
        let (width, height) = pipeline.preview.size();
        let mut result = Ok(());
        let (preview, context) = (&mut pipeline.preview, &self.context);
        self.preview
            .push_with(width, height, |dst| result = preview.read(context, dst));
        if let Err(err) = result {
            tracing::debug!(%err, "gpu share preview failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_res_timer_sleeps_short_duration() {
        let timer = HighResTimer::new();
        timer.sleep(Duration::ZERO);
        timer.sleep(Duration::from_millis(1));
    }

    #[test]
    fn output_size_fits_the_preset_and_stays_even() {
        assert_eq!(output_size((2560, 1440), (1920, 1080)), (1920, 1080));
        assert_eq!(output_size((1001, 701), (1920, 1080)), (1000, 700));
        assert_eq!(output_size((20, 10), (1920, 1080)), (MIN_SIDE, MIN_SIDE));
    }

    #[test]
    fn resize_waits_for_the_size_to_settle() {
        let t0 = Instant::now();
        let mut watch = ResizeWatch::default();
        watch.observe((800, 600), (900, 600), t0);
        assert!(!watch.due(t0 + RESIZE_SETTLE / 2));
        watch.observe((800, 600), (950, 600), t0 + RESIZE_SETTLE / 2);
        assert!(!watch.due(t0 + RESIZE_SETTLE));
        assert!(watch.due(t0 + RESIZE_SETTLE / 2 + RESIZE_SETTLE));
        assert!(!watch.due(t0 + RESIZE_SETTLE * 3));
    }

    #[test]
    fn resize_is_dropped_when_the_size_comes_back() {
        let t0 = Instant::now();
        let mut watch = ResizeWatch::default();
        watch.observe((800, 600), (900, 600), t0);
        watch.observe((800, 600), (800, 600), t0 + RESIZE_SETTLE / 2);
        assert!(!watch.due(t0 + RESIZE_SETTLE * 2));
    }

    #[test]
    fn gpu_target_maps_kinds() {
        assert_eq!(
            GpuTarget::new(CaptureKind::Screen, 1),
            Some(GpuTarget::Monitor(1))
        );
        assert_eq!(
            GpuTarget::new(CaptureKind::Window, 0x1234),
            Some(GpuTarget::Window(0x1234))
        );
        assert_eq!(GpuTarget::new(CaptureKind::Screen, u64::MAX), None);
    }

    #[test]
    fn gpu_encoder_preference_parses_known_values_and_rejects_unknown() {
        assert_eq!(
            GpuEncoderPreference::parse(""),
            Some(GpuEncoderPreference::Auto)
        );
        assert_eq!(
            GpuEncoderPreference::parse("   "),
            Some(GpuEncoderPreference::Auto)
        );
        assert_eq!(
            GpuEncoderPreference::parse("auto"),
            Some(GpuEncoderPreference::Auto)
        );
        assert_eq!(
            GpuEncoderPreference::parse(" AUTO "),
            Some(GpuEncoderPreference::Auto)
        );
        assert_eq!(
            GpuEncoderPreference::parse("mft"),
            Some(GpuEncoderPreference::Mft)
        );
        assert_eq!(
            GpuEncoderPreference::parse("MF"),
            Some(GpuEncoderPreference::Mft)
        );
        assert_eq!(
            GpuEncoderPreference::parse("Amf"),
            Some(GpuEncoderPreference::Amf)
        );
        assert_eq!(
            GpuEncoderPreference::parse("nvenc"),
            Some(GpuEncoderPreference::Nvenc)
        );
        assert_eq!(GpuEncoderPreference::parse("qsv"), None);
        assert_eq!(GpuEncoderPreference::parse("bogus"), None);
    }

    #[test]
    fn latency_recorder_computes_percentiles_and_reuses_buffer() {
        let mut recorder = LatencyRecorder::default();
        assert_eq!(
            recorder.percentiles(),
            LatencyPercentiles {
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
            }
        );

        recorder.record(Duration::from_micros(2_500));
        assert_eq!(
            recorder.percentiles(),
            LatencyPercentiles {
                p50_ms: 2.5,
                p95_ms: 2.5,
                p99_ms: 2.5,
            }
        );

        recorder.clear();
        // Record 100 samples (1.0 ms..=100.0 ms) in reverse order to verify sorting.
        for ms in (1..=100u64).rev() {
            recorder.record(Duration::from_millis(ms));
        }
        assert_eq!(
            recorder.percentiles(),
            LatencyPercentiles {
                p50_ms: 50.0,
                p95_ms: 95.0,
                p99_ms: 99.0,
            }
        );

        let capacity_before = recorder.samples_us.capacity();
        recorder.clear();
        assert!(recorder.samples_us.is_empty());
        assert_eq!(recorder.samples_us.capacity(), capacity_before);
    }

    #[test]
    fn in_flight_timestamps_match_ordered_pipelined_and_dropped_frames() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(16);
        let t2 = t0 + Duration::from_millis(32);
        let mut in_flight = InFlightTimestamps::default();

        // Pipelined 1-frame delay: frame 1 submitted before frame 0 output arrives.
        in_flight.push(1000, t0);
        in_flight.push(2000, t1);
        assert_eq!(in_flight.take(1000), Some(t0));
        assert_eq!(in_flight.take(2000), Some(t1));
        assert_eq!(in_flight.take(3000), None);

        // Dropped frame 0: output for frame 1 evicts stale frame 0.
        in_flight.push(3000, t0);
        in_flight.push(4000, t1);
        assert_eq!(in_flight.take(4000), Some(t1));
        assert_eq!(in_flight.take(4000), None);

        // MFT that rewrites timestamps still pops oldest entry in FIFO order.
        in_flight.push(5000, t1);
        in_flight.push(6000, t2);
        assert_eq!(in_flight.take(0), Some(t1));
        assert_eq!(in_flight.take(0), Some(t2));
    }
}
