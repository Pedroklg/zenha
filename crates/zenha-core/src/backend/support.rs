use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::api::AudioDevice;
use crate::audio::noise::GateReading;
use crate::backend::BackendEvent;
use crate::video::CaptureKind;

const MAX_ERROR_LEN: usize = 160;
const SILENCE_DB: f32 = -100.0;
const PCM_F32_BYTES: usize = std::mem::size_of::<f32>();

/// Drains one frame of `out.len()` little-endian `f32` PCM samples from `bytes` into `out` (`i16`).
///
/// Uses direct slice iteration when contiguous and copies only the single wrapped frame via `scratch`
/// when the ring buffer wraps mid-frame, avoiding byte-by-byte `pop_front` overhead.
pub(crate) fn drain_f32_le_to_i16(
    bytes: &mut VecDeque<u8>,
    out: &mut [i16],
    scratch: &mut Vec<u8>,
) -> bool {
    let needed = out.len() * PCM_F32_BYTES;
    if bytes.len() < needed {
        return false;
    }
    let (front, back) = bytes.as_slices();
    let src: &[u8] = if front.len() >= needed {
        &front[..needed]
    } else {
        scratch.resize(needed, 0);
        let first = front.len();
        scratch[..first].copy_from_slice(front);
        scratch[first..needed].copy_from_slice(&back[..needed - first]);
        &scratch[..needed]
    };
    for (chunk, slot) in src.chunks_exact(PCM_F32_BYTES).zip(out.iter_mut()) {
        let value =
            f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]).clamp(-1.0, 1.0) * 32767.0;
        *slot = value.round() as i16;
    }
    bytes.drain(..needed);
    true
}

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

const SPEECH_HANGOVER: Duration = Duration::from_millis(300);
/// Enhanced mic: RMS level, after noise suppression, that counts as voice when the gate is off.
pub(crate) const SPEAKING_LEVEL_DB: f32 = -45.0;
/// Platform mic: peak `audioLevel` (0..1) of the sender stats that counts as voice, about -30 dBFS.
pub(crate) const SPEAKING_AUDIO_LEVEL: f64 = 0.03;

pub(crate) fn enhanced_voice(reading: GateReading, threshold_db: Option<f32>) -> bool {
    reading.open && reading.level_db >= threshold_db.unwrap_or(SPEAKING_LEVEL_DB)
}

/// Local speaking state: voice turns it on at once, silence only after a short hangover.
#[derive(Debug, Default)]
pub(crate) struct LocalSpeech {
    speaking: bool,
    last_voice: Option<Instant>,
}

impl LocalSpeech {
    /// `silenced` (muted or deafened) turns it off at once. Returns the new state on a change.
    pub(crate) fn update(&mut self, voice: bool, silenced: bool, now: Instant) -> Option<bool> {
        if silenced {
            self.last_voice = None;
        } else if voice {
            self.last_voice = Some(now);
        }
        let speaking = self
            .last_voice
            .is_some_and(|last| now.saturating_duration_since(last) < SPEECH_HANGOVER);
        (speaking != self.speaking).then(|| {
            self.speaking = speaking;
            speaking
        })
    }
}

/// Speaking state reported by the server; the local identity is left out because it is
/// detected locally with much less delay.
#[derive(Debug, Default)]
pub(crate) struct SpeakingTracker {
    local: String,
    current: HashSet<String>,
}

impl SpeakingTracker {
    pub(crate) fn new(local: String) -> Self {
        Self {
            local,
            current: HashSet::new(),
        }
    }

    pub(crate) fn update(&mut self, mut now: HashSet<String>) -> Vec<BackendEvent> {
        now.remove(&self.local);
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
    fn speaking_tracker_ignores_the_local_identity() {
        let mut tracker = SpeakingTracker::new("me".to_owned());
        assert_eq!(tracker.update(set(&["me", "a"])), vec![speaking("a", true)]);
        assert_eq!(tracker.update(set(&["me"])), vec![speaking("a", false)]);
        assert!(tracker.update(set(&[])).is_empty());
    }

    #[test]
    fn local_speech_starts_at_once_and_stops_after_the_hangover() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut speech = LocalSpeech::default();
        assert_eq!(speech.update(false, false, ms(0)), None);
        assert_eq!(speech.update(true, false, ms(10)), Some(true));
        assert_eq!(speech.update(true, false, ms(20)), None);
        assert_eq!(speech.update(false, false, ms(200)), None);
        assert_eq!(speech.update(true, false, ms(250)), None);
        assert_eq!(speech.update(false, false, ms(540)), None);
        assert_eq!(speech.update(false, false, ms(550)), Some(false));
        assert_eq!(speech.update(false, false, ms(900)), None);
    }

    #[test]
    fn silencing_stops_local_speech_at_once() {
        let t0 = Instant::now();
        let mut speech = LocalSpeech::default();
        assert_eq!(speech.update(true, false, t0), Some(true));
        assert_eq!(speech.update(true, true, t0), Some(false));
        assert_eq!(speech.update(true, true, t0), None);
        assert_eq!(speech.update(false, false, t0), None);
        assert_eq!(speech.update(true, false, t0), Some(true));
    }

    #[test]
    fn enhanced_voice_needs_an_open_gate_and_enough_level() {
        let reading = |level_db, open| GateReading { level_db, open };
        assert!(enhanced_voice(reading(-40.0, true), None));
        assert!(!enhanced_voice(reading(-50.0, true), None));
        assert!(!enhanced_voice(reading(-20.0, false), None));
        assert!(enhanced_voice(reading(-55.0, true), Some(-60.0)));
        assert!(!enhanced_voice(reading(-35.0, true), Some(-30.0)));
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

    fn naive_pop_front_f32_le_to_i16(bytes: &mut VecDeque<u8>, out: &mut [i16]) {
        for slot in out.iter_mut() {
            let mut raw = [0u8; PCM_F32_BYTES];
            for byte in &mut raw {
                *byte = bytes.pop_front().unwrap_or(0);
            }
            let value = f32::from_le_bytes(raw).clamp(-1.0, 1.0) * 32767.0;
            *slot = value.round() as i16;
        }
    }

    fn sample_bytes(count: usize) -> Vec<u8> {
        (0..count)
            .flat_map(|i| ((i as f32 * 0.13).sin() * 1.15).to_le_bytes())
            .collect()
    }

    #[test]
    fn drain_f32_le_to_i16_matches_pop_front_for_contiguous_deque() {
        let payload = sample_bytes(24);
        let mut expected_deque = VecDeque::from(payload.clone());
        let mut actual_deque = VecDeque::from(payload);
        let mut expected = [0i16; 10];
        let mut actual = [0i16; 10];
        let mut scratch = Vec::new();

        naive_pop_front_f32_le_to_i16(&mut expected_deque, &mut expected);
        assert!(drain_f32_le_to_i16(
            &mut actual_deque,
            &mut actual,
            &mut scratch
        ));
        assert_eq!(actual, expected);
        assert_eq!(actual_deque, expected_deque);
        assert!(scratch.is_empty());
    }

    #[test]
    fn drain_f32_le_to_i16_matches_pop_front_when_wrapped_mid_sample() {
        let payload = sample_bytes(24);
        // Reserve capacity that forces a mid-frame and mid-sample wrap after push/pop.
        let mut actual_deque = VecDeque::with_capacity(63);
        for _ in 0..33 {
            actual_deque.push_back(0);
            actual_deque.pop_front();
        }
        actual_deque.extend(&payload);
        assert!(!actual_deque.as_slices().1.is_empty());

        let mut expected_deque = VecDeque::from(payload);
        let mut expected = [0i16; 10];
        let mut actual = [0i16; 10];
        let mut scratch = Vec::new();

        naive_pop_front_f32_le_to_i16(&mut expected_deque, &mut expected);
        assert!(drain_f32_le_to_i16(
            &mut actual_deque,
            &mut actual,
            &mut scratch
        ));
        assert_eq!(actual, expected);
        assert_eq!(actual_deque, expected_deque);

        // Drain a second frame to check tail alignment and scratch reuse.
        naive_pop_front_f32_le_to_i16(&mut expected_deque, &mut expected);
        assert!(drain_f32_le_to_i16(
            &mut actual_deque,
            &mut actual,
            &mut scratch
        ));
        assert_eq!(actual, expected);
        assert_eq!(actual_deque, expected_deque);

        // Fewer than `out.len() * 4` bytes remaining must leave deque untouched.
        let remaining = actual_deque.clone();
        assert!(!drain_f32_le_to_i16(
            &mut actual_deque,
            &mut actual,
            &mut scratch
        ));
        assert_eq!(actual_deque, remaining);
    }
}
