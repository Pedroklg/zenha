# 0003. Screen share on the GPU

- Status: accepted
- Date: 2026-11-16

## Context

The libwebrtc `DesktopCapturer` copies every frame to system memory. At 2560x1440 a capture took about 575 ms, so a monitor share reached under 2 fps, and the VP9 software encoder added several percent of CPU on top. The share has to reach 1080p60 on AMD, NVIDIA and Intel GPUs.

## Decision

On Windows the share stays on the GPU:

- Windows Graphics Capture (`GraphicsCaptureItem` for the monitor or the window, free-threaded frame pool) delivers D3D11 textures.
- The D3D11 video processor scales the frame to the preset and converts it to NV12 (BT.709, studio range). A second, small BGRA conversion feeds the sharer's preview.
- The Media Foundation hardware H.264 encoder (async MFT, CBR, low latency) encodes the NV12 texture directly.
- The encoded frames go to LiveKit through `NativeVideoSource::new_encoded` and `capture_encoded_frame`, published as H.264 with the pre-encoded backend.

Media Foundation is the common layer: AMD, NVIDIA and Intel ship hardware H.264 MFTs, so one code path covers the three vendors. The path is on by default; `ZENHA_GPU_SHARE=0` turns it off. If it fails to start (no hardware encoder, capture refused), the share falls back to the CPU capturer and VP9.

## Alternatives Considered

- **Vendor SDKs (NVENC, AMF, oneVPL).** Lower latency and more control, but three code paths and three SDKs to ship and test. Kept as a later option if Media Foundation falls short on one vendor.
- **Patching the libwebrtc capturer.** Still copies frames to the CPU and keeps the software encoder.
- **Software H.264 (OpenH264).** Same CPU cost as VP9 at 1080p60.

## Consequences

- CPU use during a 1080p60 share dropped from 2-9% to about 1%, measured on an RX 9060 XT.
- Viewers decode H.264 instead of VP9, usually in hardware.
- Validated on AMD only; NVIDIA and Intel need testing after the release.
- A window resize rebuilds the capture, converter and encoder, which costs one keyframe.
