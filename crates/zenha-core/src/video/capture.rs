use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::api::SessionError;
use crate::video::{CaptureKind, CaptureTarget, FrameSlot, VideoFrame, fit_within};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEnd {
    TargetClosed,
    RepeatedErrors,
    Failed(String),
}

impl fmt::Display for CaptureEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetClosed => f.write_str("the captured window or screen was closed"),
            Self::RepeatedErrors => f.write_str("capture kept failing"),
            Self::Failed(detail) => write!(f, "capture failed: {detail}"),
        }
    }
}

/// One captured image: BGRA, i.e. bytes B, G, R, A in memory, rows `stride` bytes apart.
#[derive(Debug, Clone, Copy)]
pub struct BgraFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: &'a [u8],
}

impl BgraFrame<'_> {
    pub fn is_consistent(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.stride >= self.width as usize * 4
            && self.data.len() >= self.stride * self.height as usize
    }
}

pub trait FrameSink: Send + 'static {
    fn frame(&mut self, frame: BgraFrame<'_>);
    fn dropped(&mut self, count: u64);
    fn ended(&mut self, reason: CaptureEnd);
}

pub trait FrameSource: Send + 'static {
    fn list_targets(&mut self) -> Result<Vec<CaptureTarget>, SessionError>;
    fn start(
        &mut self,
        target: &CaptureTarget,
        fps: u32,
        sink: Box<dyn FrameSink>,
    ) -> Result<(), SessionError>;
    fn stop(&mut self);

    /// Captures a single thumbnail of `target` with its longest side up to `max_side` px (RGBA).
    fn capture_thumbnail(
        &mut self,
        target: &CaptureTarget,
        max_side: u32,
        timeout: Duration,
    ) -> Result<VideoFrame, SessionError>;
}

/// Fixed-rate schedule that skips whole missed slots instead of bursting to catch up.
#[derive(Debug, Clone)]
pub struct Pacer {
    interval: Duration,
    next: Instant,
}

impl Pacer {
    pub fn new(fps: u32, start: Instant) -> Self {
        Self {
            interval: Duration::from_secs(1) / fps.max(1),
            next: start,
        }
    }

    pub fn until_next(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }

    /// Call after the work of a slot is done; returns how many slots were skipped.
    pub fn finish_slot(&mut self, now: Instant) -> u64 {
        self.next += self.interval;
        if now <= self.next {
            return 0;
        }
        let late = now - self.next;
        let skipped = u64::try_from(late.as_nanos() / self.interval.as_nanos().max(1))
            .unwrap_or(u64::MAX)
            .min(u64::from(u32::MAX));
        self.next += self.interval * u32::try_from(skipped).unwrap_or(u32::MAX);
        skipped
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        if self.thread.join().is_err() {
            tracing::error!("capture thread panicked");
        }
    }
}

const TEST_SCREEN_ID: &str = "test:screen";
const TEST_WINDOW_ID: &str = "test:window";
const BARS: [[u8; 3]; 8] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
    [20, 20, 20],
];
const SCROLL_PIXELS_PER_FRAME: usize = 6;

/// Pure Rust source producing scrolling colour bars; no OS capture involved.
pub struct TestPatternSource {
    width: u32,
    height: u32,
    worker: Option<Worker>,
}

impl TestPatternSource {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width.max(16),
            height: height.max(16),
            worker: None,
        }
    }

    /// BGRA pixels of frame number `index`, tightly packed.
    pub fn render(width: u32, height: u32, index: u64) -> Vec<u8> {
        let mut out = Vec::new();
        Self::render_into(width, height, index, &mut out);
        out
    }

    /// Writes tightly packed BGRA pixels of frame number `index` into `out`, reusing capacity.
    pub fn render_into(width: u32, height: u32, index: u64, out: &mut Vec<u8>) {
        let (w, h) = (width as usize, height as usize);
        let row_bytes = w * 4;
        out.resize(row_bytes * h, 255);
        if w == 0 || h == 0 {
            return;
        }
        let scroll = usize::try_from(index)
            .unwrap_or(0)
            .wrapping_mul(SCROLL_PIXELS_PER_FRAME);
        let band_top = h - h / 6;
        let marker = scroll % w;
        let marker_half = h / 12 + 1;

        if band_top > 0 {
            for (x, pixel) in out[..row_bytes].chunks_exact_mut(4).enumerate() {
                let bar = ((x + scroll) % w) * BARS.len() / w;
                let [r, g, b] = BARS[bar];
                pixel[0] = b;
                pixel[1] = g;
                pixel[2] = r;
                pixel[3] = 255;
            }
            for y in 1..band_top {
                out.copy_within(0..row_bytes, y * row_bytes);
            }
        }

        if band_top < h {
            let band_start = band_top * row_bytes;
            for (x, pixel) in out[band_start..band_start + row_bytes]
                .chunks_exact_mut(4)
                .enumerate()
            {
                let [r, g, b] = if x.abs_diff(marker) < marker_half {
                    [255, 255, 255]
                } else {
                    [0, 0, 0]
                };
                pixel[0] = b;
                pixel[1] = g;
                pixel[2] = r;
                pixel[3] = 255;
            }
            for y in (band_top + 1)..h {
                out.copy_within(band_start..band_start + row_bytes, y * row_bytes);
            }
        }
    }
}

impl Drop for TestPatternSource {
    fn drop(&mut self) {
        self.stop();
    }
}

impl FrameSource for TestPatternSource {
    fn list_targets(&mut self) -> Result<Vec<CaptureTarget>, SessionError> {
        Ok(vec![
            CaptureTarget {
                id: TEST_SCREEN_ID.to_owned(),
                name: "Tela de teste".to_owned(),
                kind: CaptureKind::Screen,
            },
            CaptureTarget {
                id: TEST_WINDOW_ID.to_owned(),
                name: "Janela de teste".to_owned(),
                kind: CaptureKind::Window,
            },
        ])
    }

    fn start(
        &mut self,
        target: &CaptureTarget,
        fps: u32,
        mut sink: Box<dyn FrameSink>,
    ) -> Result<(), SessionError> {
        if self.worker.is_some() {
            return Err(SessionError::ShareActive);
        }
        if target.id != TEST_SCREEN_ID && target.id != TEST_WINDOW_ID {
            return Err(SessionError::TargetNotFound(target.id.clone()));
        }
        if fps == 0 {
            return Err(SessionError::Video("fps must be positive".to_owned()));
        }
        let (width, height) = (self.width, self.height);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("test-pattern".to_owned())
            .spawn(move || {
                let mut pacer = Pacer::new(fps, Instant::now());
                let mut index = 0u64;
                let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
                while !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(pacer.until_next(Instant::now()));
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    Self::render_into(width, height, index, &mut pixels);
                    index += 1;
                    sink.frame(BgraFrame {
                        width,
                        height,
                        stride: width as usize * 4,
                        data: &pixels,
                    });
                    let skipped = pacer.finish_slot(Instant::now());
                    if skipped > 0 {
                        sink.dropped(skipped);
                    }
                }
            })
            .map_err(|err| SessionError::Internal(err.to_string()))?;
        self.worker = Some(Worker { stop, thread });
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.stop();
        }
    }

    fn capture_thumbnail(
        &mut self,
        target: &CaptureTarget,
        max_side: u32,
        _timeout: Duration,
    ) -> Result<VideoFrame, SessionError> {
        let index = match target.id.as_str() {
            TEST_SCREEN_ID => 0,
            TEST_WINDOW_ID => 15,
            _ => return Err(SessionError::TargetNotFound(target.id.clone())),
        };
        let raw = Self::render(self.width, self.height, index);
        let frame = BgraFrame {
            width: self.width,
            height: self.height,
            stride: self.width as usize * 4,
            data: &raw,
        };
        bgra_to_video_frame(&frame, max_side)
    }
}

pub fn bgra_to_rgba(frame: &BgraFrame<'_>, out: &mut [u8]) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    for y in 0..h {
        let src = &frame.data[y * frame.stride..y * frame.stride + w * 4];
        let dst = &mut out[y * w * 4..(y + 1) * w * 4];
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = 255;
        }
    }
}

/// Resizes by nearest neighbor and converts only the sampled pixels from BGRA to RGBA.
///
/// Keeps the original size when `dst_w == frame.width && dst_h == frame.height`.
pub fn bgra_to_rgba_scaled(frame: &BgraFrame<'_>, dst_w: u32, dst_h: u32, out: &mut [u8]) {
    if !frame.is_consistent() || frame.width == 0 || frame.height == 0 || dst_w == 0 || dst_h == 0 {
        return;
    }
    if dst_w == frame.width && dst_h == frame.height {
        bgra_to_rgba(frame, out);
        return;
    }
    let (src_w, src_h) = (u64::from(frame.width), u64::from(frame.height));
    let (dw, dh) = (dst_w as usize, dst_h as usize);
    let src_row_bytes = frame.width as usize * 4;
    for dy in 0..dh {
        let sy = ((dy as u64 * src_h) / dh as u64).min(src_h - 1) as usize;
        let row_start = sy * frame.stride;
        let Some(src_row) = frame.data.get(row_start..row_start + src_row_bytes) else {
            return;
        };
        let Some(dst_row) = out.get_mut(dy * dw * 4..(dy + 1) * dw * 4) else {
            return;
        };
        for (dx, d) in dst_row.chunks_exact_mut(4).enumerate() {
            let sx = (((dx as u64 * src_w) / dw as u64).min(src_w - 1) as usize) * 4;
            let Some(s) = src_row.get(sx..sx + 4) else {
                continue;
            };
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = 255;
        }
    }
}

/// Shrinks a BGRA frame to at most `max_side` on the longest side and converts it to an RGBA `VideoFrame`.
pub fn bgra_to_video_frame(
    frame: &BgraFrame<'_>,
    max_side: u32,
) -> Result<VideoFrame, SessionError> {
    if !frame.is_consistent() || frame.width == 0 || frame.height == 0 {
        return Err(SessionError::Video("quadro capturado inválido".to_owned()));
    }
    if max_side == 0 {
        return Err(SessionError::Video(
            "tamanho máximo inválido para conversão".to_owned(),
        ));
    }
    let (dst_w, dst_h) = fit_within(frame.width, frame.height, max_side, max_side);
    let mut rgba = vec![0u8; dst_w as usize * dst_h as usize * 4];
    bgra_to_rgba_scaled(frame, dst_w, dst_h, &mut rgba);
    Ok(VideoFrame {
        width: dst_w,
        height: dst_h,
        rgba,
    })
}

/// Sink that converts BGRA frames to RGBA and publishes them into a [`FrameSlot`].
pub struct SlotSink(pub FrameSlot);

impl FrameSink for SlotSink {
    fn frame(&mut self, frame: BgraFrame<'_>) {
        if !frame.is_consistent() {
            return;
        }
        self.0
            .push_with(frame.width, frame.height, |out| bgra_to_rgba(&frame, out));
    }

    fn dropped(&mut self, _count: u64) {}

    fn ended(&mut self, _reason: CaptureEnd) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Collected {
        frames: Vec<(u32, u32, usize)>,
        ended: Vec<CaptureEnd>,
    }

    struct Collect(Arc<Mutex<Collected>>);

    impl FrameSink for Collect {
        fn frame(&mut self, frame: BgraFrame<'_>) {
            assert!(frame.is_consistent());
            self.0
                .lock()
                .unwrap()
                .frames
                .push((frame.width, frame.height, frame.data.len()));
        }
        fn dropped(&mut self, _count: u64) {}
        fn ended(&mut self, reason: CaptureEnd) {
            self.0.lock().unwrap().ended.push(reason);
        }
    }

    #[test]
    fn render_has_the_right_size_and_moves() {
        let a = TestPatternSource::render(64, 36, 0);
        let b = TestPatternSource::render(64, 36, 5);
        assert_eq!(a.len(), 64 * 36 * 4);
        assert_ne!(a, b);
        assert!(a.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn first_bar_is_white_in_bgra_order() {
        let a = TestPatternSource::render(80, 40, 0);
        assert_eq!(&a[0..4], &[255, 255, 255, 255]);
        let yellow = TestPatternSource::render(80, 40, 0)[(10 * 4)..(10 * 4 + 4)].to_vec();
        assert_eq!(yellow, vec![0, 255, 255, 255]);
    }

    #[test]
    fn render_into_reuses_buffer_and_matches_pixel_formula() {
        let mut buf = Vec::new();
        for (w, h, idx) in [(64, 36, 0), (160, 90, 17), (73, 41, 9)] {
            TestPatternSource::render_into(w, h, idx, &mut buf);
            let (wu, hu) = (w as usize, h as usize);
            let scroll = (idx as usize).wrapping_mul(SCROLL_PIXELS_PER_FRAME);
            let band_top = hu - hu / 6;
            let marker = scroll % wu;
            let mut expected = vec![255u8; wu * hu * 4];
            for y in 0..hu {
                for x in 0..wu {
                    let bar = ((x + scroll) % wu) * BARS.len() / wu;
                    let [r, g, b] = if y >= band_top && x.abs_diff(marker) < hu / 12 + 1 {
                        [255, 255, 255]
                    } else if y >= band_top {
                        [0, 0, 0]
                    } else {
                        BARS[bar]
                    };
                    let at = (y * wu + x) * 4;
                    expected[at] = b;
                    expected[at + 1] = g;
                    expected[at + 2] = r;
                }
            }
            assert_eq!(buf, expected, "mismatch for {w}x{h} frame {idx}");
        }
    }

    #[test]
    fn source_delivers_frames_of_the_requested_size_and_stops() {
        let collected = Arc::new(Mutex::new(Collected::default()));
        let mut source = TestPatternSource::new(64, 48);
        let target = source.list_targets().unwrap().remove(0);
        source
            .start(&target, 200, Box::new(Collect(collected.clone())))
            .unwrap();
        assert_eq!(
            source.start(&target, 200, Box::new(Collect(collected.clone()))),
            Err(SessionError::ShareActive)
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while collected.lock().unwrap().frames.len() < 3 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        source.stop();
        let count = collected.lock().unwrap().frames.len();
        assert!(count >= 3);
        assert!(
            collected
                .lock()
                .unwrap()
                .frames
                .iter()
                .all(|f| *f == (64, 48, 64 * 48 * 4))
        );
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(collected.lock().unwrap().frames.len(), count);
    }

    #[test]
    fn unknown_target_and_zero_fps_are_rejected() {
        let mut source = TestPatternSource::new(32, 32);
        let collected = Arc::new(Mutex::new(Collected::default()));
        let bogus = CaptureTarget {
            id: "x".to_owned(),
            name: "x".to_owned(),
            kind: CaptureKind::Screen,
        };
        assert!(matches!(
            source.start(&bogus, 30, Box::new(Collect(collected.clone()))),
            Err(SessionError::TargetNotFound(_))
        ));
        let target = source.list_targets().unwrap().remove(0);
        assert!(
            source
                .start(&target, 0, Box::new(Collect(collected)))
                .is_err()
        );
    }

    #[test]
    fn slot_sink_converts_bgra_to_rgba() {
        let slot = FrameSlot::new();
        let mut sink = SlotSink(slot.clone());
        let data = [10u8, 20, 30, 0, 40, 50, 60, 0];
        sink.frame(BgraFrame {
            width: 2,
            height: 1,
            stride: 8,
            data: &data,
        });
        let mut seen = 0;
        let got = slot.take_if_newer(&mut seen).unwrap();
        assert_eq!(got.rgba, vec![30, 20, 10, 255, 60, 50, 40, 255]);
    }

    #[test]
    fn slot_sink_honours_stride_and_ignores_inconsistent_frames() {
        let slot = FrameSlot::new();
        let mut sink = SlotSink(slot.clone());
        let data = [1u8, 2, 3, 4, 99, 99, 99, 99, 5, 6, 7, 8, 99, 99, 99, 99];
        sink.frame(BgraFrame {
            width: 1,
            height: 2,
            stride: 8,
            data: &data,
        });
        sink.frame(BgraFrame {
            width: 4,
            height: 4,
            stride: 4,
            data: &data,
        });
        let mut seen = 0;
        let got = slot.take_if_newer(&mut seen).unwrap();
        assert_eq!(got.rgba, vec![3, 2, 1, 255, 7, 6, 5, 255]);
        assert_eq!(slot.generation(), 1);
    }

    #[test]
    fn pacer_skips_whole_missed_slots_only() {
        let start = Instant::now();
        let mut pacer = Pacer::new(100, start);
        assert_eq!(pacer.until_next(start), Duration::ZERO);
        assert_eq!(pacer.finish_slot(start + Duration::from_millis(4)), 0);
        assert_eq!(
            pacer.until_next(start + Duration::from_millis(4)),
            Duration::from_millis(6)
        );
        assert_eq!(pacer.finish_slot(start + Duration::from_millis(14)), 0);
        assert_eq!(pacer.finish_slot(start + Duration::from_millis(53)), 2);
        assert_eq!(pacer.finish_slot(start + Duration::from_millis(60)), 0);
    }

    #[test]
    fn bgra_to_rgba_scaled_downscales_with_stride_and_keeps_colors() {
        // 4x2 BGRA with stride padding (20 bytes/row instead of 16).
        #[rustfmt::skip]
        let data = vec![
            // row 0: pixels (B,G,R,A) then 4 padding bytes
            1, 2, 3, 0,   4, 5, 6, 0,   7, 8, 9, 0,   10, 11, 12, 0,   99, 99, 99, 99,
            // row 1
            21, 22, 23, 0,  24, 25, 26, 0,  27, 28, 29, 0,  30, 31, 32, 0,   99, 99, 99, 99,
        ];
        let frame = BgraFrame {
            width: 4,
            height: 2,
            stride: 20,
            data: &data,
        };
        let mut out = vec![0u8; 2 * 2 * 4];
        bgra_to_rgba_scaled(&frame, 2, 2, &mut out);
        assert_eq!(
            out,
            vec![
                3, 2, 1, 255, 9, 8, 7, 255, // sampled x=0, x=2 on y=0
                23, 22, 21, 255, 29, 28, 27, 255, // sampled x=0, x=2 on y=1
            ]
        );
    }

    #[test]
    fn test_pattern_thumbnail_downscales_and_rejects_unknown_target() {
        let mut source = TestPatternSource::new(1920, 1080);
        let targets = source.list_targets().unwrap();
        let thumb = source
            .capture_thumbnail(&targets[0], 320, Duration::from_secs(1))
            .unwrap();
        assert_eq!((thumb.width, thumb.height), (320, 180));
        assert!(thumb.is_valid());

        let window_thumb = source
            .capture_thumbnail(&targets[1], 320, Duration::from_secs(1))
            .unwrap();
        assert_eq!((window_thumb.width, window_thumb.height), (320, 180));
        assert_ne!(thumb.rgba, window_thumb.rgba);

        let small_thumb = source
            .capture_thumbnail(&targets[0], 4000, Duration::from_secs(1))
            .unwrap();
        assert_eq!((small_thumb.width, small_thumb.height), (1920, 1080));

        let bogus = CaptureTarget {
            id: "unknown".to_owned(),
            name: "unknown".to_owned(),
            kind: CaptureKind::Window,
        };
        assert!(matches!(
            source.capture_thumbnail(&bogus, 320, Duration::from_secs(1)),
            Err(SessionError::TargetNotFound(_))
        ));
    }
}
