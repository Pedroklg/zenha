//! Fan-out router that feeds one hardware-encoded H.264 stream to multiple independent
//! `NativeVideoSource` sinks (SFU and future P2P peers) without rate-control overwrites.

use std::sync::{Mutex, PoisonError};

use livekit::webrtc::video_frame::EncodedVideoFrame;
use livekit::webrtc::video_source::native::NativeVideoSource;

pub(crate) const SFU_SINK_ID: &str = "sfu";
pub(crate) const DEFAULT_MIN_BITRATE_BPS: u64 = 300_000;

/// Minimal sink interface abstracted so [`VideoFanOut`] can be tested without live WebRTC FFI.
pub(crate) trait EncodedVideoSink: Send {
    fn take_keyframe_request(&self) -> bool;
    fn take_rate_control_request(&self) -> Option<u64>;
    fn capture_encoded_frame(&self, frame: &EncodedVideoFrame<'_>) -> bool;
}

impl EncodedVideoSink for NativeVideoSource {
    fn take_keyframe_request(&self) -> bool {
        NativeVideoSource::take_keyframe_request(self)
    }

    fn take_rate_control_request(&self) -> Option<u64> {
        NativeVideoSource::take_rate_control_request(self).map(|req| req.target_bitrate_bps)
    }

    fn capture_encoded_frame(&self, frame: &EncodedVideoFrame<'_>) -> bool {
        NativeVideoSource::capture_encoded_frame(self, frame)
    }
}

struct VideoSink<S> {
    id: String,
    source: S,
    last_target_bps: Option<u64>,
    active: bool,
    needs_keyframe: bool,
}

/// Aggregated feedback collected from all active sinks on one encoder tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SinkControls {
    pub force_keyframe: bool,
    pub target_bitrate_bps: Option<u64>,
    pub active_sinks: usize,
}

/// Distributes pre-encoded frames to one or more independent video sinks and aggregates
/// their keyframe and bitrate requests without letting peers overwrite each other's state.
pub(crate) struct VideoFanOut<S = NativeVideoSource> {
    sinks: Mutex<Vec<VideoSink<S>>>,
    min_bitrate_bps: u64,
}

impl<S> Default for VideoFanOut<S> {
    fn default() -> Self {
        Self::new(DEFAULT_MIN_BITRATE_BPS)
    }
}

impl<S> VideoFanOut<S> {
    pub(crate) fn new(min_bitrate_bps: u64) -> Self {
        Self {
            sinks: Mutex::new(Vec::new()),
            min_bitrate_bps,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<VideoSink<S>>> {
        self.sinks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a sink as active and schedules an immediate keyframe for it.
    /// Replaces any existing sink with the same `id`.
    pub(crate) fn add(&self, id: impl Into<String>, source: S) {
        let id = id.into();
        let mut sinks = self.lock();
        if let Some(existing) = sinks.iter_mut().find(|sink| sink.id == id) {
            existing.source = source;
            existing.last_target_bps = None;
            existing.active = true;
            existing.needs_keyframe = true;
        } else {
            sinks.push(VideoSink {
                id,
                source,
                last_target_bps: None,
                active: true,
                needs_keyframe: true,
            });
        }
    }

    /// Removes the sink with `id`, returning `true` if one was present.
    pub(crate) fn remove(&self, id: &str) -> bool {
        let mut sinks = self.lock();
        let before = sinks.len();
        sinks.retain(|sink| sink.id != id);
        sinks.len() != before
    }

    /// Enables or pauses delivery and control aggregation for `id`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by upcoming P2P sink switching (T105.2)")
    )]
    pub(crate) fn set_active(&self, id: &str, active: bool) -> bool {
        let mut sinks = self.lock();
        let Some(sink) = sinks.iter_mut().find(|sink| sink.id == id) else {
            return false;
        };
        if sink.active != active {
            sink.active = active;
            sink.last_target_bps = None;
            if active {
                sink.needs_keyframe = true;
            }
        }
        true
    }

    pub(crate) fn has_active_sinks(&self) -> bool {
        self.lock().iter().any(|sink| sink.active)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

impl<S: EncodedVideoSink> VideoFanOut<S> {
    /// Polls keyframe and rate-control requests independently per active sink and computes
    /// the conservative minimum target bitrate bounded by `[min_bitrate_bps, max_bitrate_bps]`.
    pub(crate) fn poll_controls(&self, max_bitrate_bps: u64) -> SinkControls {
        let mut sinks = self.lock();
        let mut force_keyframe = false;
        let mut active_sinks = 0usize;
        let mut min_requested: Option<u64> = None;

        for sink in sinks.iter_mut().filter(|sink| sink.active) {
            active_sinks += 1;
            force_keyframe |= std::mem::take(&mut sink.needs_keyframe);
            force_keyframe |= sink.source.take_keyframe_request();
            // WebRTC reports 0 bps when a sender is paused by dynacast; ignore so it cannot
            // throttle other active sinks.
            if let Some(bps) = sink.source.take_rate_control_request()
                && bps > 0
            {
                sink.last_target_bps = Some(bps);
            }
            if let Some(target) = sink.last_target_bps {
                min_requested = Some(min_requested.map_or(target, |cur| cur.min(target)));
            }
        }

        let floor = self.min_bitrate_bps.min(max_bitrate_bps);
        let target_bitrate_bps = (active_sinks > 0).then(|| {
            min_requested.map_or(max_bitrate_bps, |bps| bps.clamp(floor, max_bitrate_bps))
        });

        SinkControls {
            force_keyframe,
            target_bitrate_bps,
            active_sinks,
        }
    }

    /// Sends one encoded access unit to every active sink; returns how many sinks received it.
    pub(crate) fn send_frame(&self, frame: &EncodedVideoFrame<'_>) -> usize {
        let sinks = self.lock();
        let mut delivered = 0;
        for sink in sinks.iter().filter(|sink| sink.active) {
            sink.source.capture_encoded_frame(frame);
            delivered += 1;
        }
        delivered
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use livekit::webrtc::video_frame::{EncodedFrameType, EncodedVideoCodec};
    use livekit::webrtc::video_source::VideoResolution;

    use super::*;

    #[derive(Default)]
    struct FakeSinkState {
        keyframe: AtomicBool,
        rate_bps: Mutex<Option<u64>>,
        frames: Mutex<Vec<(i64, bool, Vec<u8>)>>,
    }

    #[derive(Clone, Default)]
    struct FakeSink(Arc<FakeSinkState>);

    impl FakeSink {
        fn request_keyframe(&self) {
            self.0.keyframe.store(true, Ordering::SeqCst);
        }

        fn request_bitrate(&self, bps: u64) {
            *self.0.rate_bps.lock().unwrap() = Some(bps);
        }

        fn captured(&self) -> Vec<(i64, bool, Vec<u8>)> {
            self.0.frames.lock().unwrap().clone()
        }
    }

    impl EncodedVideoSink for FakeSink {
        fn take_keyframe_request(&self) -> bool {
            self.0.keyframe.swap(false, Ordering::SeqCst)
        }

        fn take_rate_control_request(&self) -> Option<u64> {
            self.0.rate_bps.lock().unwrap().take()
        }

        fn capture_encoded_frame(&self, frame: &EncodedVideoFrame<'_>) -> bool {
            self.0.frames.lock().unwrap().push((
                frame.timestamp_us,
                matches!(frame.frame_type, EncodedFrameType::Key),
                frame.payload.to_vec(),
            ));
            true
        }
    }

    fn test_frame<'a>(payload: &'a [u8], timestamp_us: i64, key: bool) -> EncodedVideoFrame<'a> {
        EncodedVideoFrame {
            codec: EncodedVideoCodec::H264,
            payload,
            timestamp_us,
            frame_type: if key {
                EncodedFrameType::Key
            } else {
                EncodedFrameType::Delta
            },
            resolution: VideoResolution {
                width: 1920,
                height: 1080,
            },
            frame_metadata: None,
        }
    }

    #[test]
    fn dynamic_add_remove_and_active_flag_control_delivery() {
        let fan_out = VideoFanOut::<FakeSink>::default();
        assert!(!fan_out.has_active_sinks());
        assert_eq!(fan_out.send_frame(&test_frame(&[1], 10, true)), 0);

        let sfu = FakeSink::default();
        let peer = FakeSink::default();
        fan_out.add(SFU_SINK_ID, sfu.clone());
        fan_out.add("peer:a", peer.clone());
        assert_eq!(fan_out.len(), 2);
        assert!(fan_out.has_active_sinks());

        assert_eq!(fan_out.send_frame(&test_frame(&[1, 2], 100, true)), 2);
        assert_eq!(sfu.captured().len(), 1);
        assert_eq!(peer.captured().len(), 1);

        assert!(fan_out.set_active(SFU_SINK_ID, false));
        assert!(!fan_out.set_active("missing", false));
        assert_eq!(fan_out.send_frame(&test_frame(&[3], 200, false)), 1);
        assert_eq!(sfu.captured().len(), 1);
        assert_eq!(peer.captured().len(), 2);

        assert!(fan_out.remove("peer:a"));
        assert!(!fan_out.remove("peer:a"));
        assert!(!fan_out.has_active_sinks());
        assert_eq!(fan_out.send_frame(&test_frame(&[4], 300, false)), 0);

        // Replacing an existing sink id does not duplicate entries and reactivates it.
        let sfu_next = FakeSink::default();
        fan_out.add(SFU_SINK_ID, sfu_next.clone());
        assert_eq!(fan_out.len(), 1);
        assert!(fan_out.has_active_sinks());
        assert_eq!(fan_out.send_frame(&test_frame(&[5], 400, true)), 1);
        assert_eq!(sfu_next.captured(), vec![(400, true, vec![5])]);
    }

    #[test]
    fn keyframe_requests_propagate_from_any_active_sink_and_on_join_or_resume() {
        let fan_out = VideoFanOut::<FakeSink>::new(300_000);
        let sfu = FakeSink::default();
        let peer = FakeSink::default();

        fan_out.add(SFU_SINK_ID, sfu.clone());
        // Newly added sink requests an initial keyframe.
        assert!(fan_out.poll_controls(8_000_000).force_keyframe);
        assert!(!fan_out.poll_controls(8_000_000).force_keyframe);

        fan_out.add("peer:a", peer.clone());
        assert!(fan_out.poll_controls(8_000_000).force_keyframe);
        assert!(!fan_out.poll_controls(8_000_000).force_keyframe);

        // PLI from either active sink triggers keyframe once.
        peer.request_keyframe();
        assert!(fan_out.poll_controls(8_000_000).force_keyframe);
        assert!(!fan_out.poll_controls(8_000_000).force_keyframe);

        sfu.request_keyframe();
        assert!(fan_out.poll_controls(8_000_000).force_keyframe);

        // Inactive sink's keyframe request is ignored until reactivated.
        assert!(fan_out.set_active("peer:a", false));
        peer.request_keyframe();
        assert!(!fan_out.poll_controls(8_000_000).force_keyframe);

        assert!(fan_out.set_active("peer:a", true));
        assert!(fan_out.poll_controls(8_000_000).force_keyframe);
        assert!(!fan_out.poll_controls(8_000_000).force_keyframe);
    }

    #[test]
    fn rate_control_aggregates_per_sink_without_overwrite_and_applies_bounds() {
        let max_bps = 8_000_000;
        let floor_bps = 300_000;
        let fan_out = VideoFanOut::<FakeSink>::new(floor_bps);
        let empty = fan_out.poll_controls(max_bps);
        assert_eq!(empty.target_bitrate_bps, None);
        assert_eq!(empty.active_sinks, 0);

        let sfu = FakeSink::default();
        let peer_a = FakeSink::default();
        let peer_b = FakeSink::default();
        fan_out.add(SFU_SINK_ID, sfu.clone());
        fan_out.add("peer:a", peer_a.clone());

        // Before any SetRates arrives, active sinks stay at the preset cap.
        let initial = fan_out.poll_controls(max_bps);
        assert_eq!(initial.target_bitrate_bps, Some(max_bps));
        assert_eq!(initial.active_sinks, 2);

        // SFU reports 6 Mbps, peer_a has not reported yet.
        sfu.request_bitrate(6_000_000);
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(6_000_000)
        );

        // peer_a reports 2 Mbps while SFU is silent: min becomes 2 Mbps.
        peer_a.request_bitrate(2_000_000);
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(2_000_000)
        );

        // SFU raises to 7.5 Mbps while peer_a stays silent: peer_a's 2 Mbps is NOT overwritten!
        sfu.request_bitrate(7_500_000);
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(2_000_000)
        );

        // A paused sender reporting 0 bps must not starve active sinks.
        sfu.request_bitrate(0);
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(2_000_000)
        );

        // A third sink below the configured floor is clamped to `floor_bps`.
        fan_out.add("peer:b", peer_b.clone());
        peer_b.request_bitrate(80_000);
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(floor_bps)
        );

        // Removing the bottleneck sink immediately recovers to the next lowest active sink.
        assert!(fan_out.remove("peer:b"));
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(2_000_000)
        );

        // Pausing peer_a recovers to SFU's last rate (7.5 Mbps).
        assert!(fan_out.set_active("peer:a", false));
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(7_500_000)
        );

        // Requests above max_bps are capped to max_bps.
        sfu.request_bitrate(15_000_000);
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(max_bps)
        );

        // Removing all rate-limited sinks while an active sink with no report remains restores max_bps.
        assert!(fan_out.set_active("peer:a", true));
        assert!(fan_out.remove(SFU_SINK_ID));
        assert_eq!(
            fan_out.poll_controls(max_bps).target_bitrate_bps,
            Some(max_bps)
        );
    }
}
