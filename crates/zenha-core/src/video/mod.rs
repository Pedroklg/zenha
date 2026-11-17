//! Vídeo: tipos de quadro e alvo, captura de tela e compartilhamento por GPU.

pub(crate) mod capture;
#[cfg(windows)]
pub(crate) mod gpu_share;

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub type Waker = Arc<dyn Fn() + Send + Sync>;

const MIN_WAKE_INTERVAL: Duration = Duration::from_millis(8);

/// Maior lado permitido para a prévia local da própria transmissão, em pixels.
pub const SHARE_PREVIEW_MAX_SIDE: u32 = 480;
/// Intervalo mínimo entre quadros da prévia local (~10 fps).
pub const SHARE_PREVIEW_INTERVAL: Duration = Duration::from_millis(100);
/// Maior lado permitido para miniaturas na escolha de tela, em pixels.
pub const CAPTURE_THUMBNAIL_MAX_SIDE: u32 = 320;
/// Tempo máximo de captura para gerar a miniatura de cada alvo.
pub const CAPTURE_THUMBNAIL_TIMEOUT: Duration = Duration::from_secs(1);

/// Limita a cadência da prévia sem acumular fila nem bloquear o produtor.
#[derive(Debug, Clone)]
pub struct PreviewGate {
    min_interval: Duration,
    last_emitted: Option<Instant>,
}

impl PreviewGate {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_emitted: None,
        }
    }

    /// Retorna `true` quando já passou o intervalo mínimo desde a última emissão.
    pub fn take(&mut self, now: Instant) -> bool {
        let due = self
            .last_emitted
            .is_none_or(|prev| now.saturating_duration_since(prev) >= self.min_interval);
        if due {
            self.last_emitted = Some(now);
        }
        due
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    Screen,
    Window,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTarget {
    pub id: String,
    pub name: String,
    pub kind: CaptureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharePreset {
    P720F30,
    #[default]
    P1080F30,
    P1080F60,
}

impl SharePreset {
    pub const ALL: [SharePreset; 3] = [Self::P720F30, Self::P1080F30, Self::P1080F60];

    pub fn max_width(self) -> u32 {
        match self {
            Self::P720F30 => 1280,
            Self::P1080F30 | Self::P1080F60 => 1920,
        }
    }

    pub fn max_height(self) -> u32 {
        match self {
            Self::P720F30 => 720,
            Self::P1080F30 | Self::P1080F60 => 1080,
        }
    }

    pub fn fps(self) -> u32 {
        match self {
            Self::P720F30 | Self::P1080F30 => 30,
            Self::P1080F60 => 60,
        }
    }

    pub fn max_bitrate_bps(self) -> u64 {
        match self {
            Self::P720F30 => 2_500_000,
            Self::P1080F30 => 5_000_000,
            Self::P1080F60 => 8_000_000,
        }
    }
}

/// Size to publish for a source of `src_w` x `src_h`: unchanged when it fits the caps,
/// otherwise scaled down keeping the aspect ratio, with even dimensions.
pub fn fit_within(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || (src_w <= max_w && src_h <= max_h) {
        return (src_w, src_h);
    }
    let (sw, sh, mw, mh) = (
        u64::from(src_w),
        u64::from(src_h),
        u64::from(max_w),
        u64::from(max_h),
    );
    let (w, h) = if mw * sh <= mh * sw {
        (mw, (sh * mw / sw).max(2))
    } else {
        ((sw * mh / sh).max(2), mh)
    };
    let even = |v: u64| u32::try_from((v & !1).max(2)).unwrap_or(2);
    (even(w), even(h))
}

/// Tightly packed RGBA8 image, `rgba.len() == width * height * 4`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl VideoFrame {
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.rgba.len() == self.width as usize * self.height as usize * 4
    }
}

#[derive(Default)]
struct SlotState {
    frame: Option<Arc<VideoFrame>>,
    generation: u64,
    last_wake: Option<Instant>,
}

struct SlotInner {
    state: Mutex<SlotState>,
    waker: Option<Waker>,
}

/// "Latest frame wins": the producer overwrites, consumers read only newer frames.
#[derive(Clone)]
pub struct FrameSlot {
    inner: Arc<SlotInner>,
}

impl fmt::Debug for FrameSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSlot")
            .field("generation", &self.generation())
            .finish()
    }
}

impl PartialEq for FrameSlot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for FrameSlot {}

impl Default for FrameSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameSlot {
    pub fn new() -> Self {
        Self::build(None)
    }

    pub fn with_waker(waker: Waker) -> Self {
        Self::build(Some(waker))
    }

    fn build(waker: Option<Waker>) -> Self {
        Self {
            inner: Arc::new(SlotInner {
                state: Mutex::new(SlotState::default()),
                waker,
            }),
        }
    }

    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn push(&self, frame: VideoFrame) {
        self.store(frame);
    }

    /// Fills a frame of the given size, reusing the previous buffer when no consumer holds it.
    pub fn push_with(&self, width: u32, height: u32, fill: impl FnOnce(&mut [u8])) {
        let recycled = self
            .lock()
            .frame
            .take()
            .and_then(|old| Arc::try_unwrap(old).ok());
        let mut frame = recycled.unwrap_or_default();
        frame.width = width;
        frame.height = height;
        frame.rgba.resize(width as usize * height as usize * 4, 0);
        fill(&mut frame.rgba);
        self.store(frame);
    }

    fn store(&self, frame: VideoFrame) {
        let wake = {
            let mut state = self.lock();
            state.frame = Some(Arc::new(frame));
            state.generation += 1;
            let now = Instant::now();
            let due = state
                .last_wake
                .is_none_or(|last| now.saturating_duration_since(last) >= MIN_WAKE_INTERVAL);
            if due {
                state.last_wake = Some(now);
            }
            due
        };
        if wake && let Some(waker) = &self.inner.waker {
            waker();
        }
    }

    /// Returns the newest frame if it is newer than `*last_seen`, and updates `last_seen`.
    pub fn take_if_newer(&self, last_seen: &mut u64) -> Option<Arc<VideoFrame>> {
        let state = self.lock();
        if state.generation <= *last_seen {
            return None;
        }
        *last_seen = state.generation;
        state.frame.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: u8) -> VideoFrame {
        VideoFrame {
            width: 2,
            height: 1,
            rgba: vec![value; 8],
        }
    }

    #[test]
    fn latest_frame_wins_and_each_frame_is_seen_once() {
        let slot = FrameSlot::new();
        let mut seen = 0;
        assert!(slot.take_if_newer(&mut seen).is_none());
        slot.push(frame(1));
        slot.push(frame(2));
        slot.push(frame(3));
        let got = slot.take_if_newer(&mut seen).unwrap();
        assert_eq!(got.rgba[0], 3);
        assert!(slot.take_if_newer(&mut seen).is_none());
        slot.push(frame(4));
        assert_eq!(slot.take_if_newer(&mut seen).unwrap().rgba[0], 4);
    }

    #[test]
    fn independent_consumers_each_see_the_newest_frame() {
        let slot = FrameSlot::new();
        slot.push(frame(9));
        let (mut a, mut b) = (0, 0);
        assert!(slot.take_if_newer(&mut a).is_some());
        assert!(slot.take_if_newer(&mut b).is_some());
        assert!(slot.take_if_newer(&mut a).is_none());
    }

    #[test]
    fn slot_does_not_grow_and_recycles_unheld_buffers() {
        let slot = FrameSlot::new();
        for i in 0..1000u32 {
            slot.push_with(4, 4, |buf| buf.fill((i % 255) as u8));
        }
        assert_eq!(slot.generation(), 1000);
        let mut seen = 0;
        let got = slot.take_if_newer(&mut seen).unwrap();
        assert!(got.is_valid());
        assert_eq!(Arc::strong_count(&got), 2);
        drop(got);
        slot.push_with(4, 4, |buf| buf.fill(7));
        let got = slot.take_if_newer(&mut seen).unwrap();
        assert_eq!(Arc::strong_count(&got), 2);
        assert_eq!(got.rgba[0], 7);
    }

    #[test]
    fn push_with_handles_size_changes() {
        let slot = FrameSlot::new();
        slot.push_with(4, 4, |buf| buf.fill(1));
        slot.push_with(2, 2, |buf| buf.fill(2));
        let mut seen = 0;
        let got = slot.take_if_newer(&mut seen).unwrap();
        assert_eq!((got.width, got.height, got.rgba.len()), (2, 2, 16));
        assert!(got.is_valid());
    }

    #[test]
    fn waker_is_rate_limited() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = count.clone();
        let slot = FrameSlot::with_waker(Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        for _ in 0..50 {
            slot.push(frame(1));
        }
        let woken = count.load(std::sync::atomic::Ordering::SeqCst);
        assert!((1..50).contains(&woken));
    }

    #[test]
    fn clones_share_the_same_slot() {
        let slot = FrameSlot::new();
        let other = slot.clone();
        assert_eq!(slot, other);
        assert_ne!(slot, FrameSlot::new());
        other.push(frame(5));
        let mut seen = 0;
        assert!(slot.take_if_newer(&mut seen).is_some());
    }

    #[test]
    fn fit_within_keeps_small_sources_and_scales_large_ones() {
        assert_eq!(fit_within(1280, 720, 1920, 1080), (1280, 720));
        assert_eq!(fit_within(1365, 767, 1920, 1080), (1365, 767));
        assert_eq!(fit_within(3840, 2160, 1920, 1080), (1920, 1080));
        assert_eq!(fit_within(2560, 1440, 1280, 720), (1280, 720));
        assert_eq!(fit_within(3440, 1440, 1920, 1080), (1920, 802));
        assert_eq!(fit_within(1080, 1920, 1920, 1080), (606, 1080));
        let (w, h) = fit_within(2001, 1001, 1920, 1080);
        assert!(w % 2 == 0 && h % 2 == 0 && w <= 1920 && h <= 1080);
    }

    #[test]
    fn presets_have_consistent_parameters() {
        assert_eq!(SharePreset::P720F30.max_height(), 720);
        assert_eq!(SharePreset::P1080F60.fps(), 60);
        assert!(SharePreset::ALL.iter().all(|p| p.max_bitrate_bps() > 0));
    }

    #[test]
    fn preview_gate_caps_rate_without_drift() {
        let mut gate = PreviewGate::new(SHARE_PREVIEW_INTERVAL);
        let t0 = Instant::now();
        assert!(gate.take(t0));
        assert!(!gate.take(t0 + Duration::from_millis(50)));
        assert!(!gate.take(t0 + Duration::from_millis(99)));
        assert!(gate.take(t0 + Duration::from_millis(100)));
        assert!(!gate.take(t0 + Duration::from_millis(150)));
        assert!(gate.take(t0 + Duration::from_millis(250)));
    }

    #[test]
    fn fit_within_respects_preview_and_thumbnail_bounds() {
        assert_eq!(
            fit_within(1920, 1080, SHARE_PREVIEW_MAX_SIDE, SHARE_PREVIEW_MAX_SIDE),
            (480, 270)
        );
        assert_eq!(
            fit_within(
                1080,
                1920,
                CAPTURE_THUMBNAIL_MAX_SIDE,
                CAPTURE_THUMBNAIL_MAX_SIDE
            ),
            (180, 320)
        );
        assert_eq!(
            fit_within(320, 200, SHARE_PREVIEW_MAX_SIDE, SHARE_PREVIEW_MAX_SIDE),
            (320, 200)
        );
    }
}
