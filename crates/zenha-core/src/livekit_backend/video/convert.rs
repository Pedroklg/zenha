//! Conversion of BGRA frames to I420, with prior downscaling when needed.

use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::video_frame::I420Buffer;

use crate::video::capture::BgraFrame;

fn write_argb_into_i420(
    src_bgra: &[u8],
    src_stride: u32,
    width: i32,
    height: i32,
    dst: &mut I420Buffer,
) {
    let (stride_y, stride_u, stride_v) = dst.strides();
    let (data_y, data_u, data_v) = dst.data_mut();
    yuv_helper::argb_to_i420(
        src_bgra, src_stride, data_y, stride_y, data_u, stride_u, data_v, stride_v, width, height,
    );
}

/// Converts BGRA (B, G, R, A in memory, which libyuv calls ARGB) directly at `out` dimensions,
/// scaling beforehand when `out` differs from the captured frame.
#[derive(Default)]
pub(super) struct BgraToI420Converter {
    scaled_bgra: Vec<u8>,
}

impl BgraToI420Converter {
    pub(super) fn convert(&mut self, frame: &BgraFrame<'_>, out: (u32, u32)) -> Option<I420Buffer> {
        if !frame.is_consistent()
            || frame.width == 0
            || frame.height == 0
            || out.0 == 0
            || out.1 == 0
        {
            return None;
        }
        let src_w = i32::try_from(frame.width).ok()?;
        let src_h = i32::try_from(frame.height).ok()?;
        let src_stride = u32::try_from(frame.stride).ok()?;
        let out_w = i32::try_from(out.0).ok()?;
        let out_h = i32::try_from(out.1).ok()?;

        if out == (frame.width, frame.height) {
            let mut buffer = I420Buffer::new(out.0, out.1);
            write_argb_into_i420(frame.data, src_stride, out_w, out_h, &mut buffer);
            return Some(buffer);
        }

        let dst_stride = out.0.checked_mul(4)?;
        let dst_bytes = usize::try_from(out.1)
            .ok()?
            .checked_mul(usize::try_from(dst_stride).ok()?)?;
        self.scaled_bgra.resize(dst_bytes, 0);
        yuv_helper::argb_scale(
            frame.data,
            src_stride,
            src_w,
            src_h,
            &mut self.scaled_bgra,
            dst_stride,
            out_w,
            out_h,
        );
        let scaled = &self.scaled_bgra[..dst_bytes];
        let mut buffer = I420Buffer::new(out.0, out.1);
        write_argb_into_i420(scaled, dst_stride, out_w, out_h, &mut buffer);
        Some(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::fit_within;
    use livekit::webrtc::video_frame::VideoBuffer;

    fn solid_bgra(width: u32, height: u32, bgra: [u8; 4]) -> Vec<u8> {
        let count = width as usize * height as usize;
        let mut data = Vec::with_capacity(count * 4);
        for _ in 0..count {
            data.extend_from_slice(&bgra);
        }
        data
    }

    #[test]
    fn converter_scales_to_preset_dimensions_before_i420() {
        let mut converter = BgraToI420Converter::default();
        let raw = solid_bgra(256, 144, [20, 180, 40, 255]);
        let frame = BgraFrame {
            width: 256,
            height: 144,
            stride: 256 * 4,
            data: &raw,
        };
        let out = fit_within(frame.width, frame.height, 128, 72);
        let i420 = converter.convert(&frame, out).unwrap();
        assert_eq!((i420.width(), i420.height()), (128, 72));
        let (y, _, _) = i420.data();
        assert!(!y.is_empty());
        assert!(converter.scaled_bgra.len() == 128 * 72 * 4);
    }

    #[test]
    fn converter_preserves_native_dimensions_without_scaling_scratch() {
        let mut converter = BgraToI420Converter::default();
        let raw = solid_bgra(64, 36, [0, 0, 255, 255]);
        let frame = BgraFrame {
            width: 64,
            height: 36,
            stride: 64 * 4,
            data: &raw,
        };
        let i420 = converter.convert(&frame, (64, 36)).unwrap();
        assert_eq!((i420.width(), i420.height()), (64, 36));
        assert!(converter.scaled_bgra.is_empty());
    }
}
