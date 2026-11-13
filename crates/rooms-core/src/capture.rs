use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::api::SessionError;
use crate::video::{CaptureKind, CaptureTarget, FrameSlot};

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
        let (w, h) = (width as usize, height as usize);
        let scroll = usize::try_from(index)
            .unwrap_or(0)
            .wrapping_mul(SCROLL_PIXELS_PER_FRAME);
        let band_top = h - h / 6;
        let marker = scroll % w.max(1);
        let mut out = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let bar = ((x + scroll) % w) * BARS.len() / w;
                let [r, g, b] = if y >= band_top && x.abs_diff(marker) < h / 12 + 1 {
                    [255, 255, 255]
                } else if y >= band_top {
                    [0, 0, 0]
                } else {
                    BARS[bar]
                };
                let at = (y * w + x) * 4;
                out[at] = b;
                out[at + 1] = g;
                out[at + 2] = r;
            }
        }
        out
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
                while !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(pacer.until_next(Instant::now()));
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let pixels = Self::render(width, height, index);
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
}
