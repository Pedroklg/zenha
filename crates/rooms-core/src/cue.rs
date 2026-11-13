//! Geração dos sons curtos de confirmação de mute e ensurdecimento.

use std::f64::consts::TAU;

const CUE_AMPLITUDE: f32 = 0.2;
const NOTE_SECONDS: f64 = 0.070;
const GAP_SECONDS: f64 = 0.020;
const RAMP_SECONDS: f64 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    Muted,
    Unmuted,
    Deafened,
    Undeafened,
}

impl Cue {
    fn frequencies(self) -> (f64, f64) {
        match self {
            Self::Unmuted => (660.0, 880.0),
            Self::Muted => (880.0, 660.0),
            Self::Undeafened => (440.0, 660.0),
            Self::Deafened => (660.0, 440.0),
        }
    }
}

/// Amostras mono em -1..1 na taxa pedida.
pub fn cue_samples(cue: Cue, sample_rate: u32) -> Vec<f32> {
    if sample_rate == 0 {
        return Vec::new();
    }
    let rate = f64::from(sample_rate);
    let note_len = (NOTE_SECONDS * rate).round() as usize;
    let gap_len = (GAP_SECONDS * rate).round() as usize;
    let ramp_len = (RAMP_SECONDS * rate).round() as usize;
    let (first_hz, second_hz) = cue.frequencies();

    let mut out = Vec::with_capacity(note_len + gap_len + note_len);
    append_note(&mut out, first_hz, rate, note_len, ramp_len);
    out.resize(out.len() + gap_len, 0.0);
    append_note(&mut out, second_hz, rate, note_len, ramp_len);
    out
}

fn append_note(out: &mut Vec<f32>, hz: f64, rate: f64, len: usize, ramp_len: usize) {
    let step = TAU * hz / rate;
    for i in 0..len {
        let edge = i.min(len - 1 - i);
        let envelope = if ramp_len > 0 && edge < ramp_len {
            edge as f32 / ramp_len as f32
        } else {
            1.0
        };
        let wave = (step * i as f64).sin() as f32;
        out.push(CUE_AMPLITUDE * envelope * wave);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 48_000;
    const NOTE_LEN: usize = 3360;
    const GAP_LEN: usize = 960;
    const RAMP_LEN: usize = 240;
    const ALL_CUES: [Cue; 4] = [Cue::Muted, Cue::Unmuted, Cue::Deafened, Cue::Undeafened];

    #[test]
    fn sample_count_matches_two_notes_and_gap_at_48khz() {
        for cue in ALL_CUES {
            let samples = cue_samples(cue, SAMPLE_RATE);
            assert_eq!(samples.len(), NOTE_LEN + GAP_LEN + NOTE_LEN);
        }
    }

    #[test]
    fn peak_amplitude_is_near_target_for_all_cues() {
        for cue in ALL_CUES {
            let samples = cue_samples(cue, SAMPLE_RATE);
            let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            assert!((0.19..=0.2).contains(&peak), "{cue:?} peak {peak}");
        }
    }

    #[test]
    fn note_edges_and_gap_are_click_free() {
        for cue in ALL_CUES {
            let samples = cue_samples(cue, SAMPLE_RATE);
            let first = samples[0].abs();
            let before_gap = samples[NOTE_LEN - 1].abs();
            let after_gap = samples[NOTE_LEN + GAP_LEN].abs();
            let last = samples[samples.len() - 1].abs();
            assert!(first < 0.01, "{cue:?} first {first}");
            assert!(before_gap < 0.01, "{cue:?} before_gap {before_gap}");
            assert!(after_gap < 0.01, "{cue:?} after_gap {after_gap}");
            assert!(last < 0.01, "{cue:?} last {last}");
            assert!(
                samples[NOTE_LEN..NOTE_LEN + GAP_LEN]
                    .iter()
                    .all(|&s| s == 0.0)
            );
        }
    }

    fn estimate_hz(note: &[f32], sample_rate: u32) -> f64 {
        let middle = &note[RAMP_LEN..note.len() - RAMP_LEN];
        let crossings = middle
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let duration = middle.len() as f64 / f64::from(sample_rate);
        (crossings as f64 / 2.0) / duration
    }

    #[test]
    fn note_frequencies_match_table_within_three_percent() {
        let expected = [
            (Cue::Unmuted, 660.0, 880.0),
            (Cue::Muted, 880.0, 660.0),
            (Cue::Undeafened, 440.0, 660.0),
            (Cue::Deafened, 660.0, 440.0),
        ];
        for (cue, first_expected, second_expected) in expected {
            let samples = cue_samples(cue, SAMPLE_RATE);
            let first_note = &samples[..NOTE_LEN];
            let second_note = &samples[NOTE_LEN + GAP_LEN..];
            let first_hz = estimate_hz(first_note, SAMPLE_RATE);
            let second_hz = estimate_hz(second_note, SAMPLE_RATE);
            assert!(
                (first_hz - first_expected).abs() / first_expected <= 0.03,
                "{cue:?} first {first_hz} vs {first_expected}"
            );
            assert!(
                (second_hz - second_expected).abs() / second_expected <= 0.03,
                "{cue:?} second {second_hz} vs {second_expected}"
            );
        }
    }

    #[test]
    fn zero_sample_rate_returns_empty() {
        for cue in ALL_CUES {
            assert!(cue_samples(cue, 0).is_empty());
        }
    }
}
