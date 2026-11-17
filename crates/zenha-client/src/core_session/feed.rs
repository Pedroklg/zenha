//! Feeds de quadros de vídeo da interface.

use std::sync::Arc;

use zenha_core::{FrameSink, FrameSlot, FrameSource, SlotSink, TestPatternSource, VideoFrame};

use crate::ui_model::{FeedHandle, FrameFeed, UiFrame};

struct Pixels(Arc<VideoFrame>);

impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.0.rgba
    }
}

struct SlotFeed(FrameSlot);

impl FrameFeed for SlotFeed {
    fn take_newer(&self, last_seen: &mut u64) -> Option<UiFrame> {
        let frame = self.0.take_if_newer(last_seen)?;
        if !frame.is_valid() {
            return None;
        }
        Some(UiFrame {
            width: frame.width as usize,
            height: frame.height as usize,
            rgba: Arc::new(Pixels(frame)),
        })
    }
}

pub(super) fn feed_handle(slot: FrameSlot) -> FeedHandle {
    FeedHandle(Arc::new(SlotFeed(slot)))
}

/// Moving colour bars written into a frame feed, for the `--demo` mode.
pub struct TestFeed {
    handle: FeedHandle,
    source: TestPatternSource,
}

impl TestFeed {
    pub fn start() -> Option<Self> {
        Self::start_with(1280, 720, 30)
    }

    pub fn start_with(width: u32, height: u32, fps: u32) -> Option<Self> {
        let slot = FrameSlot::new();
        let mut source = TestPatternSource::new(width, height);
        let target = source.list_targets().ok()?.into_iter().next()?;
        let sink: Box<dyn FrameSink> = Box::new(SlotSink(slot.clone()));
        source.start(&target, fps, sink).ok()?;
        Some(Self {
            handle: feed_handle(slot),
            source,
        })
    }

    pub fn handle(&self) -> FeedHandle {
        self.handle.clone()
    }
}

impl Drop for TestFeed {
    fn drop(&mut self) {
        self.source.stop();
    }
}
