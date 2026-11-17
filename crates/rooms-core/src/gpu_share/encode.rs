use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer,
    IMFActivate, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFMediaType, IMFSample,
    IMFShutdown, IMFTransform, METransformHaveOutput, METransformNeedInput,
    MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_EVENT_FLAG_NO_WAIT, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSampleExtension_CleanPoint, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR,
    eAVEncH264VProfile_Base,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, Interface, PWSTR};

use super::{GpuError, Step};

const OUTPUT_WAIT: Duration = Duration::from_millis(30);
const OUTPUT_POLL: Duration = Duration::from_millis(1);
const GOP_SECONDS: u32 = 10;

pub(super) struct EncodedOutput {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub time_us: i64,
}

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

fn has_sps(data: &[u8]) -> bool {
    data.windows(4)
        .any(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] & 0x1f == 7)
}

fn activate_hardware_encoder() -> Result<(IMFTransform, String), GpuError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the type infos and out pointers are live locals.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut activates,
            &mut count,
        )
    }
    .step("MFTEnumEx")?;
    if activates.is_null() || count == 0 {
        return Err(GpuError::Unsupported("no hardware H.264 encoder"));
    }
    // SAFETY: MFTEnumEx returned an array of `count` activation objects that we now own.
    let list = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
    let mut chosen = None;
    for activate in list.iter().flatten() {
        let name = friendly_name(activate);
        // SAFETY: plain COM call on a live activation object.
        match unsafe { activate.ActivateObject::<IMFTransform>() } {
            Ok(transform) => {
                chosen = Some((transform, name));
                break;
            }
            Err(err) => tracing::warn!(%err, encoder = %name, "hardware encoder failed to start"),
        }
    }
    for activate in list.iter_mut() {
        drop(activate.take());
    }
    // SAFETY: frees the array MFTEnumEx allocated, after releasing every element above.
    unsafe { CoTaskMemFree(Some(activates as *const core::ffi::c_void)) };
    chosen.ok_or(GpuError::Unsupported(
        "no hardware H.264 encoder could start",
    ))
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut text = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: the out pointers are live locals; the string is freed below.
    if unsafe { activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut text, &mut len) }
        .is_err()
    {
        return "unknown".to_owned();
    }
    // SAFETY: on success `text` is a valid NUL-terminated string allocated with CoTaskMemAlloc.
    let name = unsafe { text.to_string() }.unwrap_or_default();
    // SAFETY: frees the string allocated by GetAllocatedString.
    unsafe { CoTaskMemFree(Some(text.0 as *const core::ffi::c_void)) };
    name
}

fn set_codec(codec: &ICodecAPI, api: &GUID, value: VARIANT, what: &str) {
    // SAFETY: `api` and `value` are live; unsupported properties just return an error.
    if let Err(err) = unsafe { codec.SetValue(api, &value) } {
        tracing::debug!(%err, property = what, "encoder property not supported");
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

/// Asynchronous hardware H.264 encoder fed with NV12 textures.
pub(super) struct Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: Option<ICodecAPI>,
    name: String,
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
        let (transform, name) = activate_hardware_encoder()?;
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

        let bitrate = u32::try_from(bitrate_bps).unwrap_or(u32::MAX);
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

        let output = video_type(&MFVideoFormat_H264, size, fps)?;
        // SAFETY: plain attribute setters on the live media type.
        unsafe {
            output
                .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
                .step("average bitrate")?;
            output
                .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
                .step("profile")?;
        }
        // SAFETY: the media type is fully described above.
        unsafe { transform.SetOutputType(0, &output, 0) }.step("SetOutputType")?;
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
            codec,
            name,
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
        Ok(encoder)
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn set_bitrate(&mut self, bitrate_bps: u64) {
        let bitrate = u32::try_from(bitrate_bps).unwrap_or(u32::MAX).max(100_000);
        if bitrate == self.bitrate {
            return;
        }
        self.bitrate = bitrate;
        if let Some(codec) = &self.codec {
            set_codec(
                codec,
                &CODECAPI_AVEncCommonMeanBitRate,
                VARIANT::from(bitrate),
                "bitrate",
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
        self.pump(&mut outputs)?;
        Ok(outputs)
    }

    fn pump(&mut self, outputs: &mut Vec<EncodedOutput>) -> Result<(), GpuError> {
        loop {
            // SAFETY: non-blocking query on the transform's event queue.
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(err) if err.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(source) => {
                    return Err(GpuError::Windows {
                        step: "GetEvent",
                        source,
                    });
                }
            };
            // SAFETY: plain query on the live event.
            let kind = unsafe { event.GetType() }.step("event type")?;
            if kind == METransformNeedInput.0 as u32 {
                self.need_input += 1;
            } else if kind == METransformHaveOutput.0 as u32
                && let Some(output) = self.take_output()?
            {
                outputs.push(output);
            }
        }
    }

    /// Submits one NV12 texture; returns finished frames and whether the frame was accepted.
    pub(super) fn encode(
        &mut self,
        texture: &ID3D11Texture2D,
        timestamp: i64,
        keyframe: bool,
    ) -> Result<(Vec<EncodedOutput>, bool), GpuError> {
        let mut outputs = Vec::new();
        self.pump(&mut outputs)?;
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
        while outputs.len() == before && Instant::now() < deadline {
            std::thread::sleep(OUTPUT_POLL);
            self.pump(&mut outputs)?;
        }
        Ok((outputs, true))
    }

    fn take_output(&mut self) -> Result<Option<EncodedOutput>, GpuError> {
        let provided = if self.provides_samples {
            None
        } else {
            // SAFETY: allocates a sample with a memory buffer of the size the encoder asked for.
            unsafe {
                let buffer = MFCreateMemoryBuffer(self.output_size).step("MFCreateMemoryBuffer")?;
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
                return Ok(None);
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
        read_sample(&sample, &self.sequence_header).map(Some)
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
}
