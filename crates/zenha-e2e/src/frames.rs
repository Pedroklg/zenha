//! Received video frame statistics and acceptance criteria for the test pattern.

use serde::{Deserialize, Serialize};
use zenha_core::{TestPatternSource, VideoFrame};

pub const MIN_PLAUSIBLE_FPS: f64 = 8.0;
pub const COLOUR_TOLERANCE: f64 = 30.0;
pub const MIN_LUMA_STDDEV: f64 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameStats {
    pub width: u32,
    pub height: u32,
    pub mean_rgb: [f64; 3],
    pub luma_stddev: f64,
}

/// Mean colour and luma spread of an RGBA frame; `None` for an invalid frame.
pub fn frame_stats(frame: &VideoFrame) -> Option<FrameStats> {
    if !frame.is_valid() {
        return None;
    }
    let pixels = frame.rgba.chunks_exact(4);
    let count = pixels.len() as f64;
    let mut sum = [0.0f64; 3];
    let (mut luma_sum, mut luma_square_sum) = (0.0f64, 0.0f64);
    for pixel in pixels {
        let (r, g, b) = (
            f64::from(pixel[0]),
            f64::from(pixel[1]),
            f64::from(pixel[2]),
        );
        sum[0] += r;
        sum[1] += g;
        sum[2] += b;
        let luma = 0.299 * r + 0.587 * g + 0.114 * b;
        luma_sum += luma;
        luma_square_sum += luma * luma;
    }
    let luma_mean = luma_sum / count;
    let variance = (luma_square_sum / count - luma_mean * luma_mean).max(0.0);
    Some(FrameStats {
        width: frame.width,
        height: frame.height,
        mean_rgb: [sum[0] / count, sum[1] / count, sum[2] / count],
        luma_stddev: variance.sqrt(),
    })
}

/// Gaps between received frames: 95th percentile and worst in ms, and how many exceed 1.5x the median.
pub fn pacing(arrivals: &[f64]) -> (f64, f64, u32) {
    let mut gaps: Vec<f64> = arrivals
        .windows(2)
        .map(|w| (w[1] - w[0]) * 1000.0)
        .collect();
    if gaps.is_empty() {
        return (0.0, 0.0, 0);
    }
    gaps.sort_by(f64::total_cmp);
    let median = gaps[gaps.len() / 2];
    let p95 = gaps[(gaps.len() * 95 / 100).min(gaps.len() - 1)];
    let stutters = gaps.iter().filter(|gap| **gap > median * 1.5).count();
    (
        p95,
        gaps[gaps.len() - 1],
        u32::try_from(stutters).unwrap_or(u32::MAX),
    )
}

/// Mean RGB of the test pattern, which barely changes while the bars scroll.
pub fn expected_pattern_mean(width: u32, height: u32) -> [f64; 3] {
    let bgra = TestPatternSource::render(width, height, 0);
    let mut sum = [0.0f64; 3];
    for pixel in bgra.chunks_exact(4) {
        sum[0] += f64::from(pixel[2]);
        sum[1] += f64::from(pixel[1]);
        sum[2] += f64::from(pixel[0]);
    }
    let count = (bgra.len() / 4) as f64;
    [sum[0] / count, sum[1] / count, sum[2] / count]
}

pub fn colour_matches(measured: [f64; 3], expected: [f64; 3]) -> bool {
    measured
        .iter()
        .zip(expected)
        .all(|(m, e)| (m - e).abs() <= COLOUR_TOLERANCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: u32, height: u32, rgb: [u8; 3]) -> VideoFrame {
        let rgba = (0..width * height)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect();
        VideoFrame {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn flat_frame_has_its_colour_and_no_spread() {
        let stats = frame_stats(&flat(8, 8, [10, 20, 30])).unwrap();
        assert_eq!(stats.mean_rgb, [10.0, 20.0, 30.0]);
        assert!(stats.luma_stddev < 1e-6);
    }

    #[test]
    fn invalid_frame_has_no_stats() {
        let bad = VideoFrame {
            width: 2,
            height: 2,
            rgba: vec![0; 3],
        };
        assert!(frame_stats(&bad).is_none());
    }

    #[test]
    fn rendered_pattern_matches_the_expected_mean_and_has_spread() {
        let (w, h) = (160, 90);
        let bgra = TestPatternSource::render(w, h, 37);
        let rgba = bgra
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect();
        let stats = frame_stats(&VideoFrame {
            width: w,
            height: h,
            rgba,
        })
        .unwrap();
        assert!(colour_matches(stats.mean_rgb, expected_pattern_mean(w, h)));
        assert!(stats.luma_stddev > MIN_LUMA_STDDEV, "{stats:?}");
    }

    #[test]
    fn black_frame_does_not_match_the_pattern() {
        let expected = expected_pattern_mean(64, 36);
        assert!(!colour_matches([0.0, 0.0, 0.0], expected));
    }

    #[test]
    fn pacing_counts_gaps_well_above_the_median() {
        let arrivals = [0.0, 0.016, 0.033, 0.050, 0.100, 0.116, 0.133];
        let (p95, max, stutters) = pacing(&arrivals);
        assert!((max - 50.0).abs() < 0.01, "{max}");
        assert!((p95 - 50.0).abs() < 0.01, "{p95}");
        assert_eq!(stutters, 1);
        assert_eq!(pacing(&[1.0]), (0.0, 0.0, 0));
    }
}
