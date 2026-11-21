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

/// Computes bilinear sample indices and 8-bit weights (`w0 + w1 == 256`) for `dst_idx`.
fn bilinear_coord(dst_idx: usize, src_len: usize, dst_len: usize) -> (usize, usize, u32, u32) {
    let max_fp = (src_len.saturating_sub(1) as i64) * 256;
    let num = ((2 * dst_idx as i64 + 1) * src_len as i64 - dst_len as i64) * 128;
    let pos_fp = (num / dst_len as i64).clamp(0, max_fp);
    let i0 = (pos_fp >> 8) as usize;
    let i1 = (i0 + 1).min(src_len - 1);
    let w1 = (pos_fp & 0xFF) as u32;
    let w0 = 256 - w1;
    (i0, i1, w0, w1)
}

fn scale_bgra_bilinear(
    frame: &BgraFrame<'_>,
    dst_w: usize,
    dst_h: usize,
    dst: &mut [u8],
    x_map: &mut Vec<(usize, usize, u32, u32)>,
) {
    let src_w = frame.width as usize;
    let src_h = frame.height as usize;
    let src_row_bytes = src_w * 4;
    let dst_row_bytes = dst_w * 4;

    x_map.clear();
    x_map.reserve(dst_w);
    for dx in 0..dst_w {
        let (x0, x1, wx0, wx1) = bilinear_coord(dx, src_w, dst_w);
        x_map.push((x0 * 4, x1 * 4, wx0, wx1));
    }

    for dy in 0..dst_h {
        let (y0, y1, wy0, wy1) = bilinear_coord(dy, src_h, dst_h);
        let Some(row0) = frame
            .data
            .get(y0 * frame.stride..y0 * frame.stride + src_row_bytes)
        else {
            return;
        };
        let Some(row1) = frame
            .data
            .get(y1 * frame.stride..y1 * frame.stride + src_row_bytes)
        else {
            return;
        };
        let Some(dst_row) = dst.get_mut(dy * dst_row_bytes..(dy + 1) * dst_row_bytes) else {
            return;
        };
        for (&(x0, x1, wx0, wx1), dst_px) in x_map.iter().zip(dst_row.chunks_exact_mut(4)) {
            let (Some(p00), Some(p01), Some(p10), Some(p11)) = (
                row0.get(x0..x0 + 4),
                row0.get(x1..x1 + 4),
                row1.get(x0..x0 + 4),
                row1.get(x1..x1 + 4),
            ) else {
                continue;
            };
            for (c, out) in dst_px.iter_mut().enumerate() {
                let top = u32::from(p00[c]) * wx0 + u32::from(p01[c]) * wx1;
                let bot = u32::from(p10[c]) * wx0 + u32::from(p11[c]) * wx1;
                *out = ((top * wy0 + bot * wy1 + 32_768) >> 16) as u8;
            }
        }
    }
}

/// Converts BGRA (B, G, R, A in memory, which libyuv calls ARGB) directly at `out` dimensions,
/// scaling beforehand when `out` differs from the captured frame.
#[derive(Default)]
pub(super) struct BgraToI420Converter {
    scaled_bgra: Vec<u8>,
    x_map: Vec<(usize, usize, u32, u32)>,
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
        let out_w = i32::try_from(out.0).ok()?;
        let out_h = i32::try_from(out.1).ok()?;

        if out == (frame.width, frame.height) {
            let src_stride = u32::try_from(frame.stride).ok()?;
            let mut buffer = I420Buffer::new(out.0, out.1);
            write_argb_into_i420(frame.data, src_stride, out_w, out_h, &mut buffer);
            return Some(buffer);
        }

        let dst_w = usize::try_from(out.0).ok()?;
        let dst_h = usize::try_from(out.1).ok()?;
        let dst_stride = out.0.checked_mul(4)?;
        let dst_bytes = dst_h.checked_mul(usize::try_from(dst_stride).ok()?)?;
        self.scaled_bgra.resize(dst_bytes, 0);
        scale_bgra_bilinear(frame, dst_w, dst_h, &mut self.scaled_bgra, &mut self.x_map);
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

    #[test]
    fn bilinear_scaler_averages_channels_and_honours_stride() {
        // 4x2 BGRA with 4 bytes of stride padding per row (stride = 20).
        // Left 2x2 block averages to (50, 100, 150, 255); right 2x2 block is solid (10, 200, 30, 255).
        #[rustfmt::skip]
        let raw = vec![
            0, 50, 100, 255,   100, 150, 200, 255,   10, 200, 30, 255,   10, 200, 30, 255,   99, 99, 99, 99,
            0, 50, 100, 255,   100, 150, 200, 255,   10, 200, 30, 255,   10, 200, 30, 255,   99, 99, 99, 99,
        ];
        let frame = BgraFrame {
            width: 4,
            height: 2,
            stride: 20,
            data: &raw,
        };
        let mut dst = vec![0u8; 2 * 4];
        let mut x_map = Vec::new();
        scale_bgra_bilinear(&frame, 2, 1, &mut dst, &mut x_map);
        assert_eq!(dst, vec![50, 100, 150, 255, 10, 200, 30, 255]);
    }

    #[test]
    fn converter_preserves_color_accuracy_when_downscaling() {
        let mut converter = BgraToI420Converter::default();
        let color = [20, 180, 40, 255];
        let big = solid_bgra(128, 72, color);
        let scaled = converter
            .convert(
                &BgraFrame {
                    width: 128,
                    height: 72,
                    stride: 128 * 4,
                    data: &big,
                },
                (64, 36),
            )
            .unwrap();
        let small = solid_bgra(64, 36, color);
        let native = converter
            .convert(
                &BgraFrame {
                    width: 64,
                    height: 36,
                    stride: 64 * 4,
                    data: &small,
                },
                (64, 36),
            )
            .unwrap();
        assert_eq!(scaled.data(), native.data());
    }
}
