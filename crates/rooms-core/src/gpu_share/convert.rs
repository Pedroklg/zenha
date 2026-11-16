use std::mem::ManuallyDrop;

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::core::Interface;

use super::{GpuError, Step};

const OUTPUT_RING: usize = 4;

fn texture(
    device: &ID3D11Device,
    (width, height): (u32, u32),
    format: DXGI_FORMAT,
    bind: u32,
) -> Result<ID3D11Texture2D, GpuError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: `desc` and the out pointer are live locals; no initial data.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.step("CreateTexture2D")?;
    texture.ok_or(GpuError::Unsupported("CreateTexture2D returned no texture"))
}

/// Scales the captured BGRA frame and converts it to NV12 with the GPU video processor.
pub(super) struct Converter {
    video_context: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    input: ID3D11Texture2D,
    input_view: ID3D11VideoProcessorInputView,
    outputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    _enumerator: ID3D11VideoProcessorEnumerator,
}

impl Converter {
    pub(super) fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        source: (u32, u32),
        output: (u32, u32),
        fps: u32,
    ) -> Result<Self, GpuError> {
        let video_device: ID3D11VideoDevice = device.cast().step("ID3D11VideoDevice")?;
        let video_context: ID3D11VideoContext1 = context.cast().step("ID3D11VideoContext1")?;
        let rate = DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: source.0,
            InputHeight: source.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: `content` is a live, fully initialized description.
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content) }
            .step("CreateVideoProcessorEnumerator")?;
        // SAFETY: `enumerator` was created by this video device just above.
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }
            .step("CreateVideoProcessor")?;
        let input = texture(
            device,
            source,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        )?;
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view = None;
        // SAFETY: the texture, enumerator, description and out pointer are all live.
        unsafe {
            video_device.CreateVideoProcessorInputView(
                &input,
                &enumerator,
                &input_desc,
                Some(&mut input_view),
            )
        }
        .step("CreateVideoProcessorInputView")?;
        let input_view =
            input_view.ok_or(GpuError::Unsupported("no video processor input view"))?;
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut outputs = Vec::with_capacity(OUTPUT_RING);
        for _ in 0..OUTPUT_RING {
            let target = texture(
                device,
                output,
                DXGI_FORMAT_NV12,
                D3D11_BIND_RENDER_TARGET.0 as u32,
            )?;
            let mut view = None;
            // SAFETY: the texture, enumerator, description and out pointer are all live.
            unsafe {
                video_device.CreateVideoProcessorOutputView(
                    &target,
                    &enumerator,
                    &output_desc,
                    Some(&mut view),
                )
            }
            .step("CreateVideoProcessorOutputView")?;
            let view = view.ok_or(GpuError::Unsupported("no video processor output view"))?;
            outputs.push((target, view));
        }
        // SAFETY: plain state setters on the processor created above.
        unsafe {
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video_context.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
        }
        Ok(Self {
            video_context,
            processor,
            input,
            input_view,
            outputs,
            next: 0,
            _enumerator: enumerator,
        })
    }

    pub(super) fn input(&self) -> &ID3D11Texture2D {
        &self.input
    }

    /// Converts the current input into the next NV12 texture of the ring.
    pub(super) fn convert(&mut self) -> Result<ID3D11Texture2D, GpuError> {
        let (target, view) = &self.outputs[self.next % self.outputs.len()];
        self.next = self.next.wrapping_add(1);
        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
            ..Default::default()
        }];
        // SAFETY: the processor, output view and input view belong to this device.
        let result = unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processor, view, 0, &streams)
        };
        // SAFETY: releases the reference cloned into the stream above, exactly once.
        unsafe { ManuallyDrop::drop(&mut streams[0].pInputSurface) };
        result.step("VideoProcessorBlt")?;
        Ok(target.clone())
    }
}
