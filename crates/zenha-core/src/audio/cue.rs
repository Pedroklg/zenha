//! Generation of the short confirmation sounds (mute, deafen and join/leave).

use std::f64::consts::TAU;

const SELF_AMPLITUDE: f32 = 0.14;
const PEER_SCALE: f32 = 0.6;
const SELF_NOTE_SECONDS: f64 = 0.120;
const PEER_NOTE_SECONDS: f64 = 0.100;
const GAP_SECONDS: f64 = 0.015;
const ATTACK_SECONDS: f64 = 0.005;
const RELEASE_SECONDS: f64 = 0.008;
const DECAY_RATE: f64 = 3.5;
// Peak of sin(x) + 0.3*sin(2x) + 0.1*sin(3x) is ~1.124; keep synthesized peak bounded by target amplitude.
const HARMONIC_NORM: f64 = 1.125;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    Muted,
    Unmuted,
    Deafened,
    Undeafened,
    SelfJoined,
    SelfLeft,
    PeerJoined,
    PeerLeft,
    IntegrationPost,
}

struct CueSpec {
    notes_hz: &'static [f64],
    note_seconds: f64,
    amplitude: f32,
}

impl Cue {
    fn spec(self) -> CueSpec {
        match self {
            Self::Unmuted => CueSpec {
                notes_hz: &[660.0, 880.0],
                note_seconds: SELF_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE,
            },
            Self::Muted => CueSpec {
                notes_hz: &[880.0, 660.0],
                note_seconds: SELF_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE,
            },
            Self::Undeafened => CueSpec {
                notes_hz: &[440.0, 660.0],
                note_seconds: SELF_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE,
            },
            Self::Deafened => CueSpec {
                notes_hz: &[660.0, 440.0],
                note_seconds: SELF_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE,
            },
            Self::SelfJoined => CueSpec {
                notes_hz: &[523.25, 659.25, 783.99],
                note_seconds: SELF_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE,
            },
            Self::SelfLeft => CueSpec {
                notes_hz: &[783.99, 659.25, 523.25],
                note_seconds: SELF_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE,
            },
            Self::PeerJoined => CueSpec {
                notes_hz: &[329.63, 392.0],
                note_seconds: PEER_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE * PEER_SCALE,
            },
            Self::PeerLeft => CueSpec {
                notes_hz: &[392.0, 329.63],
                note_seconds: PEER_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE * PEER_SCALE,
            },
            Self::IntegrationPost => CueSpec {
                notes_hz: &[987.77, 1318.51],
                note_seconds: PEER_NOTE_SECONDS,
                amplitude: SELF_AMPLITUDE * PEER_SCALE,
            },
        }
    }
}

/// Mono samples in -1..1 at the requested rate.
pub fn cue_samples(cue: Cue, sample_rate: u32) -> Vec<f32> {
    if sample_rate == 0 {
        return Vec::new();
    }
    let spec = cue.spec();
    let rate = f64::from(sample_rate);
    let note_len = (spec.note_seconds * rate).round() as usize;
    let gap_len = (GAP_SECONDS * rate).round() as usize;
    let attack_len = (ATTACK_SECONDS * rate).round() as usize;
    let release_len = (RELEASE_SECONDS * rate).round() as usize;

    let note_count = spec.notes_hz.len();
    let total_len = note_count * note_len + note_count.saturating_sub(1) * gap_len;
    let mut out = Vec::with_capacity(total_len);
    for (idx, &hz) in spec.notes_hz.iter().enumerate() {
        if idx > 0 {
            out.resize(out.len() + gap_len, 0.0);
        }
        append_note(
            &mut out,
            hz,
            rate,
            note_len,
            attack_len,
            release_len,
            spec.amplitude,
        );
    }
    out
}

fn append_note(
    out: &mut Vec<f32>,
    hz: f64,
    rate: f64,
    len: usize,
    attack_len: usize,
    release_len: usize,
    amplitude: f32,
) {
    if len == 0 {
        return;
    }
    let step = TAU * hz / rate;
    let decay_span = len.saturating_sub(attack_len).max(1) as f64;
    for i in 0..len {
        let attack = if attack_len > 0 && i < attack_len {
            i as f64 / attack_len as f64
        } else {
            1.0
        };
        let remaining = len - 1 - i;
        let release = if release_len > 0 && remaining < release_len {
            remaining as f64 / release_len as f64
        } else {
            1.0
        };
        let progress = i.saturating_sub(attack_len) as f64 / decay_span;
        let fundamental_decay = (-DECAY_RATE * progress).exp();
        let harmonic_decay = (-2.0 * DECAY_RATE * progress).exp();

        let phase = step * i as f64;
        let tone = phase.sin()
            + 0.3 * harmonic_decay * (2.0 * phase).sin()
            + 0.1 * harmonic_decay * (3.0 * phase).sin();
        let envelope = (attack * release * fundamental_decay) as f32;
        out.push(amplitude * envelope * (tone / HARMONIC_NORM) as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 48_000;
    const ALL_CUES: [Cue; 9] = [
        Cue::Muted,
        Cue::Unmuted,
        Cue::Deafened,
        Cue::Undeafened,
        Cue::SelfJoined,
        Cue::SelfLeft,
        Cue::PeerJoined,
        Cue::PeerLeft,
        Cue::IntegrationPost,
    ];

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max)
    }

    #[test]
    fn duration_is_within_expected_range_for_all_cues() {
        for cue in ALL_CUES {
            let samples = cue_samples(cue, SAMPLE_RATE);
            let spec = cue.spec();
            let note_len = (spec.note_seconds * f64::from(SAMPLE_RATE)).round() as usize;
            let gap_len = (GAP_SECONDS * f64::from(SAMPLE_RATE)).round() as usize;
            let expected = spec.notes_hz.len() * note_len + (spec.notes_hz.len() - 1) * gap_len;
            assert_eq!(samples.len(), expected, "{cue:?}");

            let duration_ms = samples.len() as f64 / f64::from(SAMPLE_RATE) * 1000.0;
            assert!(
                (180.0..=500.0).contains(&duration_ms),
                "{cue:?} duration {duration_ms}ms out of range"
            );
        }
        assert!(
            cue_samples(Cue::PeerJoined, SAMPLE_RATE).len()
                < cue_samples(Cue::SelfJoined, SAMPLE_RATE).len()
        );
        assert!(
            cue_samples(Cue::PeerLeft, SAMPLE_RATE).len()
                < cue_samples(Cue::SelfLeft, SAMPLE_RATE).len()
        );
    }

    #[test]
    fn peak_amplitude_is_softer_and_peers_are_quieter_than_self() {
        let mut self_min = f32::MAX;
        let mut peer_max = 0.0f32;
        for cue in ALL_CUES {
            let samples = cue_samples(cue, SAMPLE_RATE);
            let p = peak(&samples);
            match cue {
                Cue::PeerJoined | Cue::PeerLeft | Cue::IntegrationPost => {
                    assert!((0.06..=0.095).contains(&p), "{cue:?} peak {p}");
                    peer_max = peer_max.max(p);
                }
                _ => {
                    assert!((0.12..=0.155).contains(&p), "{cue:?} peak {p}");
                    self_min = self_min.min(p);
                }
            }
        }
        assert!(
            peer_max < self_min * 0.7,
            "peer_max {peer_max} vs self_min {self_min}"
        );
    }

    #[test]
    fn note_edges_and_gaps_are_click_free() {
        let gap_len = (GAP_SECONDS * f64::from(SAMPLE_RATE)).round() as usize;
        for cue in ALL_CUES {
            let samples = cue_samples(cue, SAMPLE_RATE);
            let spec = cue.spec();
            let note_len = (spec.note_seconds * f64::from(SAMPLE_RATE)).round() as usize;
            assert!(samples[0].abs() < 0.001, "{cue:?} first {}", samples[0]);
            assert!(
                samples[samples.len() - 1].abs() < 0.001,
                "{cue:?} last {}",
                samples[samples.len() - 1]
            );
            for note_idx in 0..spec.notes_hz.len() - 1 {
                let gap_start = (note_idx + 1) * note_len + note_idx * gap_len;
                let gap_end = gap_start + gap_len;
                assert!(samples[gap_start - 1].abs() < 0.001);
                assert!(samples[gap_end].abs() < 0.001);
                assert!(samples[gap_start..gap_end].iter().all(|&s| s == 0.0));
            }
        }
    }

    fn estimate_hz(note: &[f32], sample_rate: u32) -> f64 {
        let trim =
            ((ATTACK_SECONDS.max(RELEASE_SECONDS)) * f64::from(sample_rate)).round() as usize;
        let middle = &note[trim..note.len() - trim];
        let crossings = middle
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let duration = middle.len() as f64 / f64::from(sample_rate);
        (crossings as f64 / 2.0) / duration
    }

    #[test]
    fn note_frequencies_match_table_within_three_percent() {
        let gap_len = (GAP_SECONDS * f64::from(SAMPLE_RATE)).round() as usize;
        for cue in ALL_CUES {
            let spec = cue.spec();
            let note_len = (spec.note_seconds * f64::from(SAMPLE_RATE)).round() as usize;
            let samples = cue_samples(cue, SAMPLE_RATE);
            for (idx, &expected_hz) in spec.notes_hz.iter().enumerate() {
                let start = idx * (note_len + gap_len);
                let measured = estimate_hz(&samples[start..start + note_len], SAMPLE_RATE);
                assert!(
                    (measured - expected_hz).abs() / expected_hz <= 0.03,
                    "{cue:?} note {idx}: {measured} vs {expected_hz}"
                );
            }
        }
    }

    #[test]
    fn all_cues_produce_distinct_waveforms() {
        let rendered: Vec<(Cue, Vec<f32>)> = ALL_CUES
            .iter()
            .map(|&cue| (cue, cue_samples(cue, SAMPLE_RATE)))
            .collect();
        for (i, (cue_a, a)) in rendered.iter().enumerate() {
            for (cue_b, b) in &rendered[i + 1..] {
                assert_ne!(a, b, "{cue_a:?} and {cue_b:?} produced identical samples");
            }
        }
    }

    #[test]
    fn zero_sample_rate_returns_empty() {
        for cue in ALL_CUES {
            assert!(cue_samples(cue, 0).is_empty());
        }
    }
}
