use std::mem::ManuallyDrop;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{DXGI_ADAPTER_DESC, IDXGIDevice};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncH264CABACEnable, CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer,
    IMFActivate, IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFAttributes,
    IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFShutdown,
    IMFTransform, METransformHaveOutput, METransformNeedInput, MF_E_SHUTDOWN,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_TRANSFORM_ASYNC_UNLOCK, MFASYNC_CALLBACK_QUEUE_MULTITHREADED,
    MFASYNC_FAST_IO_PROCESSING_CALLBACK, MFCreateAttributes, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSampleExtension_CleanPoint, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_FLUSH,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnum2, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_Base,
    eAVEncH264VProfile_Main,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, IUnknown, IUnknownImpl, Interface, PWSTR, Ref, implement};

use super::{GpuError, Step};

const OUTPUT_WAIT: Duration = Duration::from_millis(30);
const INITIAL_INPUT_WAIT: Duration = Duration::from_millis(5);
const MIN_ENCODER_BITRATE_BPS: u32 = 100_000;
const GOP_SECONDS: u32 = 10;

pub(super) struct EncodedOutput {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub time_us: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum H264Profile {
    Main,
    Baseline,
}

impl H264Profile {
    fn codec_label(self) -> &'static str {
        match self {
            Self::Main => "h264/main",
            Self::Baseline => "h264/baseline",
        }
    }
}

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

fn has_sps(data: &[u8]) -> bool {
    data.windows(4)
        .any(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] & 0x1f == 7)
}

fn luid_bytes(luid: LUID) -> [u8; 8] {
    let low = luid.LowPart.to_ne_bytes();
    let high = luid.HighPart.to_ne_bytes();
    [
        low[0], low[1], low[2], low[3], high[0], high[1], high[2], high[3],
    ]
}

fn vendor_id_matches(attr: &str, vendor_id: u32) -> bool {
    let trimmed = attr.trim();
    let hex = trimmed
        .strip_prefix("VEN_")
        .or_else(|| trimmed.strip_prefix("ven_"))
        .or_else(|| {
            trimmed
                .get(..4)
                .filter(|prefix| prefix.eq_ignore_ascii_case("VEN_"))
                .and_then(|_| trimmed.get(4..))
        });
    let Some(hex) = hex else {
        return false;
    };
    u32::from_str_radix(hex, 16).is_ok_and(|parsed| parsed == vendor_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateInfo {
    luid: Option<[u8; 8]>,
    vendor: Option<String>,
}

/// Orders candidate indices: exact adapter LUID first, then vendor ID, preserving MF's order.
fn rank_candidates(
    candidates: &[CandidateInfo],
    target_luid: Option<[u8; 8]>,
    target_vendor: Option<u32>,
) -> Vec<usize> {
    let mut ranked: Vec<(u8, usize)> = candidates
        .iter()
        .enumerate()
        .map(|(idx, info)| {
            let tier = if target_luid.is_some() && info.luid == target_luid {
                0
            } else if let (Some(vendor), Some(attr)) = (target_vendor, info.vendor.as_deref())
                && vendor_id_matches(attr, vendor)
            {
                1
            } else {
                2
            };
            (tier, idx)
        })
        .collect();
    ranked.sort_by_key(|&(tier, idx)| (tier, idx));
    ranked.into_iter().map(|(_, idx)| idx).collect()
}

fn adapter_desc(device: &ID3D11Device) -> Result<DXGI_ADAPTER_DESC, GpuError> {
    let dxgi: IDXGIDevice = device.cast().step("IDXGIDevice")?;
    // SAFETY: `dxgi` is a live DXGI device queried from `device`.
    let adapter = unsafe { dxgi.GetAdapter() }.step("GetAdapter")?;
    // SAFETY: `adapter` is a live DXGI adapter returned by `GetAdapter`.
    unsafe { adapter.GetDesc() }.step("GetDesc")
}

fn take_activates(activates: *mut Option<IMFActivate>, count: u32) -> Vec<IMFActivate> {
    if activates.is_null() {
        return Vec::new();
    }
    // SAFETY: MFTEnum2/MFTEnumEx returned an array of `count` activation objects that we own.
    let list = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
    let owned: Vec<IMFActivate> = list.iter_mut().filter_map(Option::take).collect();
    // SAFETY: frees the array Media Foundation allocated, after taking every element above.
    unsafe { CoTaskMemFree(Some(activates as *const core::ffi::c_void)) };
    owned
}

fn enumerate_by_luid(
    input: &MFT_REGISTER_TYPE_INFO,
    output: &MFT_REGISTER_TYPE_INFO,
    luid: [u8; 8],
) -> Result<Vec<IMFActivate>, GpuError> {
    let mut attrs: Option<IMFAttributes> = None;
    // SAFETY: out pointer is a live local.
    unsafe { MFCreateAttributes(&mut attrs, 1) }.step("MFCreateAttributes")?;
    let attrs = attrs.ok_or(GpuError::Unsupported("no attribute store"))?;
    // SAFETY: `MFT_ENUM_ADAPTER_LUID` expects the 8-byte binary representation of `LUID`.
    unsafe { attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid) }.step("SetBlob(LUID)")?;
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: type infos, attributes and out pointers are live locals.
    unsafe {
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(input),
            Some(output),
            &attrs,
            &mut activates,
            &mut count,
        )
    }
    .step("MFTEnum2")?;
    Ok(take_activates(activates, count))
}

fn enumerate_all_hardware(
    input: &MFT_REGISTER_TYPE_INFO,
    output: &MFT_REGISTER_TYPE_INFO,
) -> Result<Vec<IMFActivate>, GpuError> {
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the type infos and out pointers are live locals.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(input),
            Some(output),
            &mut activates,
            &mut count,
        )
    }
    .step("MFTEnumEx")?;
    Ok(take_activates(activates, count))
}

fn read_luid_attribute(activate: &IMFActivate) -> Option<[u8; 8]> {
    let mut buf = [0u8; 8];
    let mut written = 0u32;
    // SAFETY: `buf` holds `size_of::<LUID>()` bytes and `written` is a live local.
    let ok = unsafe { activate.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut buf, Some(&mut written)) };
    (ok.is_ok() && written == 8).then_some(buf)
}

fn allocated_string(activate: &IMFActivate, key: &GUID) -> Option<String> {
    let mut text = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: the out pointers are live locals; the string is freed below.
    unsafe { activate.GetAllocatedString(key, &mut text, &mut len) }.ok()?;
    // SAFETY: on success `text` is a valid NUL-terminated string allocated with CoTaskMemAlloc.
    let value = unsafe { text.to_string() }.ok();
    // SAFETY: frees the string allocated by GetAllocatedString.
    unsafe { CoTaskMemFree(Some(text.0 as *const core::ffi::c_void)) };
    value
}

fn friendly_name(activate: &IMFActivate) -> String {
    allocated_string(activate, &MFT_FRIENDLY_NAME_Attribute)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn try_activate_ranked(
    activates: &[IMFActivate],
    target_luid: Option<[u8; 8]>,
    target_vendor: Option<u32>,
) -> Option<(IMFTransform, String)> {
    let infos: Vec<CandidateInfo> = activates
        .iter()
        .map(|activate| CandidateInfo {
            luid: read_luid_attribute(activate),
            vendor: allocated_string(activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute),
        })
        .collect();
    for idx in rank_candidates(&infos, target_luid, target_vendor) {
        let activate = &activates[idx];
        let name = friendly_name(activate);
        // SAFETY: plain COM call on a live activation object.
        match unsafe { activate.ActivateObject::<IMFTransform>() } {
            Ok(transform) => return Some((transform, name)),
            Err(err) => tracing::warn!(%err, encoder = %name, "hardware encoder failed to start"),
        }
    }
    None
}

fn activate_hardware_encoder(device: &ID3D11Device) -> Result<(IMFTransform, String), GpuError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let (target_luid, target_vendor) = match adapter_desc(device) {
        Ok(desc) => (Some(luid_bytes(desc.AdapterLuid)), Some(desc.VendorId)),
        Err(err) => {
            tracing::warn!(%err, "could not query DXGI adapter description for MFT matching");
            (None, None)
        }
    };
    if let Some(luid) = target_luid {
        match enumerate_by_luid(&input, &output, luid) {
            Ok(activates) if !activates.is_empty() => {
                if let Some(found) = try_activate_ranked(&activates, Some(luid), target_vendor) {
                    return Ok(found);
                }
            }
            Ok(_) => {
                tracing::debug!(
                    vendor_id = target_vendor.unwrap_or(0),
                    "no hardware encoder matched the adapter LUID, falling back to MFTEnumEx"
                );
            }
            Err(err) => {
                tracing::warn!(%err, "enumerating encoders by adapter LUID failed, falling back to MFTEnumEx");
            }
        }
    }
    let activates = enumerate_all_hardware(&input, &output)?;
    if activates.is_empty() {
        return Err(GpuError::Unsupported("no hardware H.264 encoder"));
    }
    try_activate_ranked(&activates, target_luid, target_vendor).ok_or(GpuError::Unsupported(
        "no hardware H.264 encoder could start",
    ))
}

fn set_codec(codec: &ICodecAPI, api: &GUID, value: VARIANT, what: &str) -> bool {
    // SAFETY: `api` and `value` are live; unsupported properties return an error.
    match unsafe { codec.SetValue(api, &value) } {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(%err, property = what, "encoder codec property failed");
            false
        }
    }
}

fn video_type(subtype: &GUID, size: (u32, u32), fps: u32) -> Result<IMFMediaType, GpuError> {
    // SAFETY: creates an empty media type.
    let media = unsafe { MFCreateMediaType() }.step("MFCreateMediaType")?;
    // SAFETY: plain attribute setters on the live media type.
    unsafe {
        media
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .step("major type")?;
        media.SetGUID(&MF_MT_SUBTYPE, subtype).step("subtype")?;
        media
            .SetUINT64(&MF_MT_FRAME_SIZE, pack(size.0, size.1))
            .step("frame size")?;
        media
            .SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))
            .step("frame rate")?;
        media
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
            .step("aspect ratio")?;
        media
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .step("interlace mode")?;
    }
    Ok(media)
}

fn h264_output_type(
    size: (u32, u32),
    fps: u32,
    bitrate: u32,
    profile: H264Profile,
) -> Result<IMFMediaType, GpuError> {
    let output = video_type(&MFVideoFormat_H264, size, fps)?;
    let profile_id = match profile {
        H264Profile::Main => eAVEncH264VProfile_Main.0 as u32,
        H264Profile::Baseline => eAVEncH264VProfile_Base.0 as u32,
    };
    // SAFETY: plain attribute setters on the live media type.
    unsafe {
        output
            .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
            .step("average bitrate")?;
        output
            .SetUINT32(&MF_MT_MPEG2_PROFILE, profile_id)
            .step("profile")?;
    }
    Ok(output)
}

#[derive(Default)]
struct EventState {
    need_input: u32,
    have_output: u32,
    error: Option<windows::core::Error>,
    closed: bool,
}

#[derive(Default)]
struct EventInbox {
    state: Mutex<EventState>,
    cvar: Condvar,
}

impl EventInbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, EventState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records one MF event type and returns whether `BeginGetEvent` should be re-armed.
    fn record_event(&self, kind: windows::core::Result<u32>) -> bool {
        let mut notify = false;
        let should_rearm = {
            let mut state = self.lock();
            if state.closed {
                false
            } else {
                match kind {
                    Ok(k) if k == METransformNeedInput.0 as u32 => {
                        state.need_input = state.need_input.saturating_add(1);
                        notify = true;
                        true
                    }
                    Ok(k) if k == METransformHaveOutput.0 as u32 => {
                        state.have_output = state.have_output.saturating_add(1);
                        notify = true;
                        true
                    }
                    Ok(_) => true,
                    Err(err) => {
                        state.error = Some(err);
                        notify = true;
                        false
                    }
                }
            }
        };
        if notify {
            self.cvar.notify_all();
        }
        should_rearm
    }

    fn record_error(&self, err: windows::core::Error) {
        {
            let mut state = self.lock();
            if err.code() == MF_E_SHUTDOWN {
                state.closed = true;
            } else if !state.closed {
                state.error = Some(err);
            }
        }
        self.cvar.notify_all();
    }

    fn close(&self) {
        {
            let mut state = self.lock();
            state.closed = true;
        }
        self.cvar.notify_all();
    }

    fn take_counts(&self) -> Result<(u32, u32, bool), GpuError> {
        let mut state = self.lock();
        if let Some(source) = state.error.take() {
            return Err(GpuError::Windows {
                step: "EndGetEvent",
                source,
            });
        }
        let need_input = std::mem::take(&mut state.need_input);
        let have_output = std::mem::take(&mut state.have_output);
        Ok((need_input, have_output, state.closed))
    }

    fn wait_for_output(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while state.have_output == 0 && state.error.is_none() && !state.closed {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (guard, result) = self
                .cvar
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
            if result.timed_out() {
                break;
            }
        }
    }

    fn wait_for_input(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while state.need_input == 0 && state.error.is_none() && !state.closed {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (guard, result) = self
                .cvar
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
            if result.timed_out() {
                break;
            }
        }
    }
}

/// Forwards Media Foundation events to [`EventInbox`] and re-arms `BeginGetEvent`.
#[implement(IMFAsyncCallback)]
struct EventCallback {
    inbox: Arc<EventInbox>,
}

impl EventCallback {
    fn into_interface(inbox: Arc<EventInbox>) -> IMFAsyncCallback {
        Self { inbox }.into()
    }
}

impl IMFAsyncCallback_Impl for EventCallback_Impl {
    fn GetParameters(&self, flags: *mut u32, queue: *mut u32) -> windows::core::Result<()> {
        // SAFETY: Media Foundation passes valid out pointers; null ones are skipped.
        unsafe {
            if let Some(flags) = flags.as_mut() {
                *flags = MFASYNC_FAST_IO_PROCESSING_CALLBACK;
            }
            if let Some(queue) = queue.as_mut() {
                *queue = MFASYNC_CALLBACK_QUEUE_MULTITHREADED;
            }
        }
        Ok(())
    }

    fn Invoke(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        let result = result.ok()?;
        // SAFETY: `BeginGetEvent` passes the event generator as the state object.
        let state = unsafe { result.GetState() }?;
        let events: IMFMediaEventGenerator = state.cast()?;
        // SAFETY: completes the pending `BeginGetEvent` for `result`.
        let rearm = match unsafe { events.EndGetEvent(result) } {
            // SAFETY: plain query on the event returned by `EndGetEvent`.
            Ok(event) => self.inbox.record_event(unsafe { event.GetType() }),
            Err(err) => {
                self.inbox.record_error(err);
                false
            }
        };
        if rearm {
            let callback: IMFAsyncCallback = self.to_interface();
            // SAFETY: re-arms the next event on the live generator with no lock held.
            if let Err(err) = unsafe { events.BeginGetEvent(&callback, &state) } {
                self.inbox.record_error(err);
            }
        }
        Ok(())
    }
}

/// Asynchronous hardware H.264 encoder fed with NV12 textures.
pub(super) struct Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    inbox: Arc<EventInbox>,
    codec: Option<ICodecAPI>,
    name: String,
    profile: H264Profile,
    need_input: u32,
    provides_samples: bool,
    output_size: u32,
    sequence_header: Vec<u8>,
    frame_duration: i64,
    bitrate: u32,
    _manager: IMFDXGIDeviceManager,
}

impl Encoder {
    pub(super) fn new(
        device: &ID3D11Device,
        size: (u32, u32),
        fps: u32,
        bitrate_bps: u64,
    ) -> Result<Self, GpuError> {
        let (transform, name) = activate_hardware_encoder(device)?;
        // SAFETY: attribute store of the live transform.
        let attributes = unsafe { transform.GetAttributes() }.step("encoder attributes")?;
        // SAFETY: plain attribute setters; a synchronous encoder fails the cast below instead.
        unsafe {
            let _ = attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
        }
        let events: IMFMediaEventGenerator = transform
            .cast()
            .map_err(|_| GpuError::Unsupported("the hardware encoder is not asynchronous"))?;
        let mut token = 0u32;
        let mut manager = None;
        // SAFETY: the out pointers are live locals.
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }
            .step("MFCreateDXGIDeviceManager")?;
        let manager = manager.ok_or(GpuError::Unsupported("no DXGI device manager"))?;
        // SAFETY: binds our live device to the manager with the token it returned.
        unsafe { manager.ResetDevice(device, token) }.step("ResetDevice")?;
        // SAFETY: the transform keeps its own reference to the manager it receives.
        unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) }
            .step("set D3D manager")?;

        let bitrate = u32::try_from(bitrate_bps)
            .unwrap_or(u32::MAX)
            .max(MIN_ENCODER_BITRATE_BPS);
        let codec: Option<ICodecAPI> = transform.cast().ok();
        if let Some(codec) = &codec {
            set_codec(
                codec,
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
                "rate control",
            );
            set_codec(
                codec,
                &CODECAPI_AVEncCommonMeanBitRate,
                VARIANT::from(bitrate),
                "bitrate",
            );
            set_codec(
                codec,
                &CODECAPI_AVLowLatencyMode,
                VARIANT::from(true),
                "low latency",
            );
            set_codec(
                codec,
                &CODECAPI_AVEncMPVGOPSize,
                VARIANT::from(fps * GOP_SECONDS),
                "gop size",
            );
            set_codec(
                codec,
                &CODECAPI_AVEncMPVDefaultBPictureCount,
                VARIANT::from(0u32),
                "b frames",
            );
        }

        let output_main = h264_output_type(size, fps, bitrate, H264Profile::Main)?;
        // SAFETY: the media type is fully described above.
        let profile = match unsafe { transform.SetOutputType(0, &output_main, 0) } {
            Ok(()) => {
                if let Some(codec) = &codec {
                    set_codec(
                        codec,
                        &CODECAPI_AVEncH264CABACEnable,
                        VARIANT::from(true),
                        "cabac",
                    );
                }
                H264Profile::Main
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    encoder = %name,
                    "h264 main profile rejected, falling back to baseline"
                );
                let output_base = h264_output_type(size, fps, bitrate, H264Profile::Baseline)?;
                // SAFETY: the fallback media type is fully described above.
                unsafe { transform.SetOutputType(0, &output_base, 0) }.step("SetOutputType")?;
                H264Profile::Baseline
            }
        };

        let input = video_type(&MFVideoFormat_NV12, size, fps)?;
        // SAFETY: the media type is fully described above.
        unsafe { transform.SetInputType(0, &input, 0) }.step("SetInputType")?;
        // SAFETY: plain query on the configured transform.
        let info = unsafe { transform.GetOutputStreamInfo(0) }.step("GetOutputStreamInfo")?;
        let provides_samples = info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                as u32
            != 0;
        let mut encoder = Self {
            transform,
            events,
            inbox: Arc::new(EventInbox::default()),
            codec,
            name,
            profile,
            need_input: 0,
            provides_samples,
            output_size: info.cbSize.max(size.0 * size.1),
            sequence_header: Vec::new(),
            frame_duration: 10_000_000 / i64::from(fps.max(1)),
            bitrate,
            _manager: manager,
        };
        encoder.refresh_sequence_header();
        // SAFETY: streaming messages on the configured transform.
        unsafe {
            encoder
                .transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .step("flush")?;
            encoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .step("begin streaming")?;
            encoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .step("start of stream")?;
        }
        encoder.start_events()?;
        Ok(encoder)
    }

    fn start_events(&self) -> Result<(), GpuError> {
        let callback = EventCallback::into_interface(Arc::clone(&self.inbox));
        let state: IUnknown = self.events.clone().into();
        // SAFETY: starts async event delivery; the callback stops re-arming once `Drop` closes `inbox`.
        unsafe { self.events.BeginGetEvent(&callback, &state) }.step("BeginGetEvent")?;
        self.inbox.wait_for_input(INITIAL_INPUT_WAIT);
        Ok(())
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn codec_label(&self) -> &'static str {
        self.profile.codec_label()
    }

    pub(super) fn set_bitrate(&mut self, bitrate_bps: u64) {
        let bitrate = u32::try_from(bitrate_bps)
            .unwrap_or(u32::MAX)
            .max(MIN_ENCODER_BITRATE_BPS);
        if bitrate == self.bitrate {
            return;
        }
        self.bitrate = bitrate;
        let Some(codec) = &self.codec else {
            tracing::warn!(
                encoder = %self.name,
                bitrate_bps = bitrate,
                "dynamic bitrate update ignored: encoder does not expose ICodecAPI"
            );
            return;
        };
        if !set_codec(
            codec,
            &CODECAPI_AVEncCommonMeanBitRate,
            VARIANT::from(bitrate),
            "bitrate",
        ) {
            tracing::warn!(
                encoder = %self.name,
                bitrate_bps = bitrate,
                "dynamic bitrate update rejected by encoder"
            );
        }
    }

    fn refresh_sequence_header(&mut self) {
        // SAFETY: plain query on the configured transform.
        let Ok(current) = (unsafe { self.transform.GetOutputCurrentType(0) }) else {
            return;
        };
        // SAFETY: plain query on the live media type.
        let Ok(len) = (unsafe { current.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) }) else {
            return;
        };
        let mut header = vec![0u8; len as usize];
        // SAFETY: `header` has exactly the size the media type reported.
        if unsafe { current.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut header, None) }.is_ok() {
            self.sequence_header = header;
        }
    }

    /// Collects pending events without blocking; returns any finished frames.
    pub(super) fn poll(&mut self) -> Result<Vec<EncodedOutput>, GpuError> {
        let mut outputs = Vec::new();
        let _ = self.drain_events(&mut outputs)?;
        Ok(outputs)
    }

    /// Moves arrived async events into encoder state and collects any ready outputs.
    /// Returns `true` if the event stream was shut down.
    fn drain_events(&mut self, outputs: &mut Vec<EncodedOutput>) -> Result<bool, GpuError> {
        let (need_input, have_output, closed) = self.inbox.take_counts()?;
        self.need_input = self.need_input.saturating_add(need_input);
        for _ in 0..have_output {
            if let Some(output) = self.take_output()? {
                outputs.push(output);
            }
        }
        Ok(closed)
    }

    /// Submits one NV12 texture; returns finished frames and whether the frame was accepted.
    pub(super) fn encode(
        &mut self,
        texture: &ID3D11Texture2D,
        timestamp: i64,
        keyframe: bool,
    ) -> Result<(Vec<EncodedOutput>, bool), GpuError> {
        let mut outputs = Vec::new();
        let _ = self.drain_events(&mut outputs)?;
        if self.need_input == 0 {
            return Ok((outputs, false));
        }
        if keyframe && let Some(codec) = &self.codec {
            set_codec(
                codec,
                &CODECAPI_AVEncVideoForceKeyFrame,
                VARIANT::from(1u32),
                "keyframe",
            );
        }
        // SAFETY: wraps our live NV12 texture in a media buffer that holds its own reference.
        let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false) }
            .step("MFCreateDXGISurfaceBuffer")?;
        let plane: IMF2DBuffer = buffer.cast().step("IMF2DBuffer")?;
        // SAFETY: plain queries and setters on live Media Foundation objects.
        let sample = unsafe {
            let length = plane.GetContiguousLength().step("contiguous length")?;
            buffer.SetCurrentLength(length).step("buffer length")?;
            let sample = MFCreateSample().step("MFCreateSample")?;
            sample.AddBuffer(&buffer).step("AddBuffer")?;
            sample.SetSampleTime(timestamp).step("sample time")?;
            sample
                .SetSampleDuration(self.frame_duration)
                .step("sample duration")?;
            sample
        };
        // SAFETY: the encoder asked for input, and the sample keeps the texture alive.
        unsafe { self.transform.ProcessInput(0, &sample, 0) }.step("ProcessInput")?;
        self.need_input -= 1;
        let deadline = Instant::now() + OUTPUT_WAIT;
        let before = outputs.len();
        while outputs.len() == before {
            let closed = self.drain_events(&mut outputs)?;
            if outputs.len() > before || closed {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            self.inbox.wait_for_output(remaining);
        }
        Ok((outputs, true))
    }

    fn take_output(&mut self) -> Result<Option<EncodedOutput>, GpuError> {
        for _ in 0..2 {
            let provided = if self.provides_samples {
                None
            } else {
                // SAFETY: allocates a sample with a memory buffer of the size the encoder asked for.
                unsafe {
                    let buffer =
                        MFCreateMemoryBuffer(self.output_size).step("MFCreateMemoryBuffer")?;
                    let sample = MFCreateSample().step("MFCreateSample")?;
                    sample.AddBuffer(&buffer).step("AddBuffer")?;
                    Some(sample)
                }
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(provided),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: `buffers` holds one entry for output stream 0, as the transform expects.
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            // SAFETY: takes back ownership of the references in the output buffer, exactly once.
            let (sample, _events) = unsafe {
                (
                    ManuallyDrop::take(&mut buffers[0].pSample),
                    ManuallyDrop::take(&mut buffers[0].pEvents),
                )
            };
            match result {
                Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // SAFETY: renegotiates the output type the encoder now proposes.
                    unsafe {
                        let proposed = self
                            .transform
                            .GetOutputAvailableType(0, 0)
                            .step("GetOutputAvailableType")?;
                        self.transform
                            .SetOutputType(0, &proposed, 0)
                            .step("SetOutputType after stream change")?;
                    }
                    self.refresh_sequence_header();
                    // Some async MFTs deliver the renegotiated sample on an immediate retry.
                    continue;
                }
                Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(source) => {
                    return Err(GpuError::Windows {
                        step: "ProcessOutput",
                        source,
                    });
                }
                Ok(()) => {}
            }
            let Some(sample) = sample else {
                return Ok(None);
            };
            return read_sample(&sample, &self.sequence_header).map(Some);
        }
        Ok(None)
    }
}

fn read_sample(sample: &IMFSample, sequence_header: &[u8]) -> Result<EncodedOutput, GpuError> {
    // SAFETY: plain query on the live sample.
    let keyframe =
        unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.is_ok_and(|value| value != 0);
    // SAFETY: plain query on the live sample.
    let time_us = unsafe { sample.GetSampleTime() }.unwrap_or(0) / 10;
    // SAFETY: plain query on the live sample.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.step("ConvertToContiguousBuffer")?;
    let mut data_ptr = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: locks the buffer; it stays locked until the matching Unlock below.
    unsafe { buffer.Lock(&mut data_ptr, None, Some(&mut len)) }.step("Lock")?;
    let mut data = Vec::with_capacity(len as usize + sequence_header.len());
    if !data_ptr.is_null() {
        // SAFETY: Lock returned a pointer to `len` readable bytes, valid until Unlock.
        let bytes = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
        if keyframe && !has_sps(bytes) {
            data.extend_from_slice(sequence_header);
        }
        data.extend_from_slice(bytes);
    }
    // SAFETY: balances the successful Lock above.
    let _ = unsafe { buffer.Unlock() };
    Ok(EncodedOutput {
        data,
        keyframe,
        time_us,
    })
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.inbox.close();
        // SAFETY: ends streaming and shuts the asynchronous transform down before release.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_sps_finds_only_sequence_parameter_sets() {
        assert!(has_sps(&[0, 0, 0, 1, 0x67, 0x42]));
        assert!(!has_sps(&[0, 0, 0, 1, 0x65, 0x88]));
        assert!(!has_sps(&[]));
    }

    #[test]
    fn pack_puts_the_first_value_in_the_high_half() {
        assert_eq!(pack(1920, 1080), (1920u64 << 32) | 1080);
    }

    #[test]
    fn luid_bytes_matches_luid_size_and_endianness() {
        assert_eq!(size_of::<LUID>(), 8);
        let luid = LUID {
            LowPart: 0x0102_0304,
            HighPart: 0x0506_0708,
        };
        let bytes = luid_bytes(luid);
        assert_eq!(&bytes[..4], &luid.LowPart.to_ne_bytes());
        assert_eq!(&bytes[4..], &luid.HighPart.to_ne_bytes());
    }

    #[test]
    fn vendor_id_matches_hex_prefix_case_insensitively() {
        assert!(vendor_id_matches("VEN_1002", 0x1002));
        assert!(vendor_id_matches("ven_10de", 0x10DE));
        assert!(vendor_id_matches("  Ven_8086  ", 0x8086));
        assert!(!vendor_id_matches("VEN_8086", 0x1002));
        assert!(!vendor_id_matches("1002", 0x1002));
        assert!(!vendor_id_matches("VEN_ZZZZ", 0x1002));
        assert!(!vendor_id_matches("", 0x1002));
    }

    #[test]
    fn rank_candidates_prefers_luid_then_vendor_then_original_order() {
        let luid_a = [1, 0, 0, 0, 0, 0, 0, 0];
        let luid_b = [2, 0, 0, 0, 0, 0, 0, 0];
        let candidates = vec![
            CandidateInfo {
                luid: None,
                vendor: Some("VEN_8086".to_owned()),
            },
            CandidateInfo {
                luid: Some(luid_a),
                vendor: Some("VEN_1002".to_owned()),
            },
            CandidateInfo {
                luid: None,
                vendor: Some("VEN_1002".to_owned()),
            },
            CandidateInfo {
                luid: Some(luid_b),
                vendor: Some("VEN_1002".to_owned()),
            },
        ];
        assert_eq!(
            rank_candidates(&candidates, Some(luid_b), Some(0x1002)),
            vec![3, 1, 2, 0]
        );
        assert_eq!(rank_candidates(&candidates, None, None), vec![0, 1, 2, 3]);
    }

    #[test]
    fn event_inbox_tracks_events_wakes_waiter_and_handles_shutdown() {
        let inbox = Arc::new(EventInbox::default());
        assert!(inbox.record_event(Ok(METransformNeedInput.0 as u32)));
        assert!(inbox.record_event(Ok(METransformHaveOutput.0 as u32)));
        assert_eq!(inbox.take_counts().unwrap(), (1, 1, false));
        assert_eq!(inbox.take_counts().unwrap(), (0, 0, false));

        let producer = Arc::clone(&inbox);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            producer.record_event(Ok(METransformHaveOutput.0 as u32));
        });
        inbox.wait_for_output(Duration::from_secs(1));
        handle.join().unwrap();
        assert_eq!(inbox.take_counts().unwrap(), (0, 1, false));

        inbox.record_error(windows::core::Error::from(MF_E_SHUTDOWN));
        assert_eq!(inbox.take_counts().unwrap(), (0, 0, true));
        assert!(!inbox.record_event(Ok(METransformNeedInput.0 as u32)));
    }

    #[test]
    fn event_callback_reports_parameters_and_releases_the_inbox() {
        let inbox = Arc::new(EventInbox::default());
        let callback = EventCallback::into_interface(Arc::clone(&inbox));
        assert_eq!(Arc::strong_count(&inbox), 2);

        let mut flags = 0u32;
        let mut queue = 0u32;
        // SAFETY: `callback` is a live `IMFAsyncCallback` and out pointers are live locals.
        unsafe { callback.GetParameters(&mut flags, &mut queue) }.unwrap();
        assert_eq!(flags, MFASYNC_FAST_IO_PROCESSING_CALLBACK);
        assert_eq!(queue, MFASYNC_CALLBACK_QUEUE_MULTITHREADED);

        let unknown: IUnknown = callback.cast().unwrap();
        let agile: windows::Win32::System::Com::IAgileObject = unknown.cast().unwrap();
        assert!(callback.cast::<IMFTransform>().is_err());

        let cloned = callback.clone();
        drop(agile);
        drop(unknown);
        drop(callback);
        assert_eq!(Arc::strong_count(&inbox), 2);
        drop(cloned);
        assert_eq!(Arc::strong_count(&inbox), 1);
    }
}
