use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::api::AudioDevice;
use crate::backend::BackendEvent;
use crate::noise::GateReading;
use crate::video::CaptureKind;

const MAX_ERROR_LEN: usize = 160;
const SILENCE_DB: f32 = -100.0;

/// Stored threshold meaning "gate off"; never inside the valid -70..=-20 range.
pub(crate) const GATE_OFF: i8 = 0;
const METER_FRAMES: u32 = 10;

pub(crate) fn gate_threshold(raw: i8) -> Option<f32> {
    (raw != GATE_OFF).then(|| f32::from(raw))
}

/// Reports the loudest level of every `METER_FRAMES` frames, with the latest gate state.
#[derive(Default)]
pub(crate) struct LevelMeter {
    frames: u32,
    peak: Option<f32>,
}

impl LevelMeter {
    pub(crate) fn push(&mut self, reading: GateReading) -> Option<(i8, bool)> {
        self.peak = Some(
            self.peak
                .map_or(reading.level_db, |p| p.max(reading.level_db)),
        );
        self.frames += 1;
        if self.frames < METER_FRAMES {
            return None;
        }
        let peak = self.peak.take().unwrap_or(SILENCE_DB);
        self.frames = 0;
        Some((peak.round().clamp(SILENCE_DB, 0.0) as i8, reading.open))
    }
}

pub(crate) fn display_name(name: String, identity: &str) -> String {
    if name.trim().is_empty() {
        identity.to_owned()
    } else {
        name
    }
}

pub(crate) fn audio_device(id: &str, name: &str) -> AudioDevice {
    AudioDevice {
        id: id.to_owned(),
        name: display_name(name.to_owned(), id),
    }
}

pub(crate) fn unique_devices(devices: Vec<AudioDevice>) -> Vec<AudioDevice> {
    let mut seen = HashSet::new();
    devices
        .into_iter()
        .filter(|device| seen.insert(device.id.clone()))
        .collect()
}

pub(crate) fn target_id(kind: CaptureKind, source_id: u64) -> String {
    let prefix = match kind {
        CaptureKind::Screen => "screen",
        CaptureKind::Window => "window",
    };
    format!("{prefix}:{source_id}")
}

pub(crate) fn parse_target_id(id: &str) -> Option<(CaptureKind, u64)> {
    let (prefix, number) = id.split_once(':')?;
    let kind = match prefix {
        "screen" => CaptureKind::Screen,
        "window" => CaptureKind::Window,
        _ => return None,
    };
    Some((kind, number.parse().ok()?))
}

/// Display name for a capture source; untitled windows are not worth listing.
pub(crate) fn target_name(title: &str, kind: CaptureKind, index: usize) -> Option<String> {
    let title = title.trim();
    match (kind, title.is_empty()) {
        (_, false) => Some(title.to_owned()),
        (CaptureKind::Screen, true) => Some(format!("Tela {}", index + 1)),
        (CaptureKind::Window, true) => None,
    }
}

pub(crate) fn sanitize_error(message: &str, secret: &str) -> String {
    let cleaned = if secret.is_empty() {
        message.to_owned()
    } else {
        message.replace(secret, "<redacted>")
    };
    cleaned.chars().take(MAX_ERROR_LEN).collect()
}

const METER_WINDOW: Duration = Duration::from_secs(5);
const MAX_ERROR_STREAK: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MeterReport {
    pub frames: u64,
    pub dropped: u64,
    pub avg_ms: f64,
    pub window_secs: f64,
}

/// Counts frames and the average time spent per frame, reporting once per window.
#[derive(Debug)]
pub(crate) struct ShareMeter {
    window_start: Instant,
    frames: u64,
    dropped: u64,
    busy: Duration,
}

impl ShareMeter {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            frames: 0,
            dropped: 0,
            busy: Duration::ZERO,
        }
    }

    pub(crate) fn record_frame(&mut self, busy: Duration) {
        self.frames += 1;
        self.busy += busy;
    }

    pub(crate) fn record_dropped(&mut self, count: u64) {
        self.dropped += count;
    }

    pub(crate) fn report(&mut self, now: Instant) -> Option<MeterReport> {
        let elapsed = now.saturating_duration_since(self.window_start);
        if elapsed < METER_WINDOW {
            return None;
        }
        let avg_ms = if self.frames == 0 {
            0.0
        } else {
            self.busy.as_secs_f64() * 1000.0 / self.frames as f64
        };
        let report = MeterReport {
            frames: self.frames,
            dropped: self.dropped,
            avg_ms,
            window_secs: elapsed.as_secs_f64(),
        };
        *self = Self::new(now);
        Some(report)
    }
}

/// Gives up on a capture only after temporary errors persist without any good frame.
#[derive(Debug, Default)]
pub(crate) struct CaptureHealth {
    first_error: Option<Instant>,
}

impl CaptureHealth {
    pub(crate) fn good_frame(&mut self) {
        self.first_error = None;
    }

    /// Returns true when the error streak is long enough to end the capture.
    pub(crate) fn temporary_error(&mut self, now: Instant) -> bool {
        let first = *self.first_error.get_or_insert(now);
        now.saturating_duration_since(first) >= MAX_ERROR_STREAK
    }
}

#[derive(Debug, Default)]
pub(crate) struct SpeakingTracker {
    current: HashSet<String>,
}

impl SpeakingTracker {
    pub(crate) fn update(&mut self, now: HashSet<String>) -> Vec<BackendEvent> {
        let mut started: Vec<String> = now.difference(&self.current).cloned().collect();
        let mut stopped: Vec<String> = self.current.difference(&now).cloned().collect();
        started.sort();
        stopped.sort();
        self.current = now;
        let stopped = stopped
            .into_iter()
            .map(|identity| BackendEvent::SpeakingChanged {
                identity,
                speaking: false,
            });
        let started = started
            .into_iter()
            .map(|identity| BackendEvent::SpeakingChanged {
                identity,
                speaking: true,
            });
        stopped.chain(started).collect()
    }

    pub(crate) fn forget(&mut self, identity: &str) {
        self.current.remove(identity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_reports_the_peak_every_ten_frames() {
        let mut meter = LevelMeter::default();
        let reading = |level_db, open| GateReading { level_db, open };
        for i in 0..9 {
            assert_eq!(meter.push(reading(-50.0 + i as f32, false)), None);
        }
        assert_eq!(meter.push(reading(-60.4, true)), Some((-42, true)));
        for _ in 0..9 {
            assert_eq!(meter.push(reading(-100.0, true)), None);
        }
        assert_eq!(meter.push(reading(3.0, false)), Some((0, false)));
    }

    #[test]
    fn gate_threshold_maps_the_off_sentinel_to_none() {
        assert_eq!(gate_threshold(GATE_OFF), None);
        assert_eq!(gate_threshold(-45), Some(-45.0));
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn speaking(identity: &str, speaking: bool) -> BackendEvent {
        BackendEvent::SpeakingChanged {
            identity: identity.to_owned(),
            speaking,
        }
    }

    #[test]
    fn display_name_falls_back_to_identity_when_blank() {
        assert_eq!(display_name(String::new(), "ana"), "ana");
        assert_eq!(display_name("  ".to_owned(), "ana"), "ana");
        assert_eq!(display_name("Ana".to_owned(), "ana"), "Ana");
    }

    #[test]
    fn sanitize_error_hides_the_secret_and_truncates() {
        assert_eq!(
            sanitize_error("bad url ws://h?access_token=abc.def", "abc.def"),
            "bad url ws://h?access_token=<redacted>"
        );
        assert_eq!(sanitize_error("plain", ""), "plain");
        assert_eq!(
            sanitize_error(&"x".repeat(500), "").chars().count(),
            MAX_ERROR_LEN
        );
    }

    #[test]
    fn speaking_emits_only_transitions() {
        let mut tracker = SpeakingTracker::default();
        assert_eq!(
            tracker.update(set(&["a", "b"])),
            vec![speaking("a", true), speaking("b", true)]
        );
        assert!(tracker.update(set(&["a", "b"])).is_empty());
        assert_eq!(
            tracker.update(set(&["b", "c"])),
            vec![speaking("a", false), speaking("c", true)]
        );
        assert_eq!(
            tracker.update(set(&[])),
            vec![speaking("b", false), speaking("c", false)]
        );
        assert!(tracker.update(set(&[])).is_empty());
    }

    #[test]
    fn forgetting_a_participant_emits_nothing_and_allows_a_fresh_start() {
        let mut tracker = SpeakingTracker::default();
        tracker.update(set(&["a"]));
        tracker.forget("a");
        assert_eq!(tracker.update(set(&["a"])), vec![speaking("a", true)]);
    }

    #[test]
    fn target_ids_round_trip_and_reject_garbage() {
        for kind in [CaptureKind::Screen, CaptureKind::Window] {
            assert_eq!(parse_target_id(&target_id(kind, 42)), Some((kind, 42)));
        }
        for bad in ["", "screen", "screen:", "screen:x", "tela:1", ":1"] {
            assert_eq!(parse_target_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn untitled_windows_are_skipped_and_screens_are_numbered() {
        assert_eq!(target_name(" ", CaptureKind::Window, 0), None);
        assert_eq!(
            target_name("", CaptureKind::Screen, 1).as_deref(),
            Some("Tela 2")
        );
        assert_eq!(
            target_name(" Bloco de notas ", CaptureKind::Window, 0).as_deref(),
            Some("Bloco de notas")
        );
    }

    #[test]
    fn meter_reports_once_per_window_and_resets() {
        let t0 = Instant::now();
        let mut meter = ShareMeter::new(t0);
        meter.record_frame(Duration::from_millis(4));
        meter.record_frame(Duration::from_millis(8));
        meter.record_dropped(3);
        assert!(meter.report(t0 + Duration::from_secs(4)).is_none());
        let report = meter.report(t0 + Duration::from_secs(5)).unwrap();
        assert_eq!((report.frames, report.dropped), (2, 3));
        assert!((report.avg_ms - 6.0).abs() < 1e-9);
        assert!(meter.report(t0 + Duration::from_secs(6)).is_none());
        let empty = meter.report(t0 + Duration::from_secs(10)).unwrap();
        assert_eq!((empty.frames, empty.avg_ms), (0, 0.0));
    }

    #[test]
    fn capture_health_gives_up_only_after_a_long_error_streak() {
        let t0 = Instant::now();
        let mut health = CaptureHealth::default();
        assert!(!health.temporary_error(t0));
        assert!(!health.temporary_error(t0 + Duration::from_secs(9)));
        health.good_frame();
        assert!(!health.temporary_error(t0 + Duration::from_secs(12)));
        assert!(health.temporary_error(t0 + Duration::from_secs(22)));
    }

    #[test]
    fn audio_device_name_falls_back_to_the_id() {
        assert_eq!(audio_device("g1", "Mic").name, "Mic");
        assert_eq!(audio_device("g1", " ").name, "g1");
    }

    #[test]
    fn duplicate_device_ids_keep_the_first_entry() {
        let devices = vec![
            audio_device("a", "First"),
            audio_device("b", "Other"),
            audio_device("a", "Again"),
        ];
        assert_eq!(
            unique_devices(devices),
            vec![audio_device("a", "First"), audio_device("b", "Other")]
        );
    }
}
