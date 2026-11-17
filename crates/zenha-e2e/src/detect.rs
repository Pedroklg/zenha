//! Deciding whether "the tone is playing" from probe measurements, always against a baseline.

use serde::Serialize;

use crate::dsp::ToneMeasurement;

/// Test tone frequency: a quarter tone between D# and E, outside the notes of common music.
pub const TONE_HZ: f64 = 1290.0;
/// Peak amplitude (fraction of full scale); RMS of about 0.021, or -33.5 dBFS.
pub const TONE_AMPLITUDE: f64 = 0.03;
/// Absolute floor of the tone RMS: 10 times below the expected level, leaving margin for app volume and Opus.
pub const MIN_TONE_RMS: f64 = 0.002;
/// The tone must exceed the baseline by 5 times (14 dB), which rules out music on the PC.
pub const BASELINE_FACTOR: f64 = 5.0;
/// Minimum fraction of total power in the tone (1%, or -20 dB); only fails with background sound above -13 dBFS.
pub const MIN_TONE_RATIO: f64 = 0.01;
/// Absence: the tone drops below 15% of the level measured when present.
pub const ABSENT_FRACTION: f64 = 0.15;
/// Above this total RMS in the baseline the report warns that sound is playing on the PC.
pub const NOISY_BASELINE_RMS: f64 = 0.02;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    pub ok: bool,
    pub reason: String,
}

fn verdict(ok: bool, reason: impl Into<String>) -> Verdict {
    Verdict {
        ok,
        reason: reason.into(),
    }
}

/// True when `measured` shows the test tone, given what was playing before it started.
pub fn tone_present(baseline: &ToneMeasurement, measured: &ToneMeasurement) -> Verdict {
    if measured.tone_rms < MIN_TONE_RMS {
        return verdict(
            false,
            format!(
                "tone_rms {:.5} below the floor {MIN_TONE_RMS}",
                measured.tone_rms
            ),
        );
    }
    let needed = BASELINE_FACTOR * baseline.tone_rms;
    if measured.tone_rms < needed {
        return verdict(
            false,
            format!(
                "tone_rms {:.5} not {BASELINE_FACTOR}x above the baseline {:.5}",
                measured.tone_rms, baseline.tone_rms
            ),
        );
    }
    if measured.tone_ratio < MIN_TONE_RATIO {
        return verdict(
            false,
            format!(
                "tone_ratio {:.4} below {MIN_TONE_RATIO} (other sound dominates)",
                measured.tone_ratio
            ),
        );
    }
    verdict(
        true,
        format!(
            "tone_rms {:.5} (baseline {:.5}), tone_ratio {:.3}",
            measured.tone_rms, baseline.tone_rms, measured.tone_ratio
        ),
    )
}

/// True when the tone is gone: far below the level seen while it played, or below the floor.
pub fn tone_absent(
    baseline: &ToneMeasurement,
    when_present: &ToneMeasurement,
    measured: &ToneMeasurement,
) -> Verdict {
    let limit = (ABSENT_FRACTION * when_present.tone_rms)
        .max(1.5 * baseline.tone_rms)
        .max(MIN_TONE_RMS);
    if measured.tone_rms <= limit {
        verdict(
            true,
            format!(
                "tone_rms {:.5} within the limit {limit:.5}",
                measured.tone_rms
            ),
        )
    } else {
        verdict(
            false,
            format!(
                "tone_rms {:.5} above the limit {limit:.5}",
                measured.tone_rms
            ),
        )
    }
}

pub fn baseline_is_noisy(baseline: &ToneMeasurement) -> bool {
    baseline.total_rms > NOISY_BASELINE_RMS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(tone_rms: f64, total_rms: f64) -> ToneMeasurement {
        ToneMeasurement {
            hz: TONE_HZ,
            sample_rate: 48_000,
            samples: 24_000,
            total_rms,
            tone_rms,
            tone_ratio: if total_rms > 0.0 {
                (tone_rms / total_rms).powi(2).min(1.0)
            } else {
                0.0
            },
            peak: total_rms * 2.0,
            coverage: 1.0,
        }
    }

    #[test]
    fn expected_tone_level_passes_on_a_quiet_pc() {
        let silent = ToneMeasurement::silent(TONE_HZ, 48_000);
        assert!(tone_present(&silent, &m(0.0212, 0.0212)).ok);
    }

    #[test]
    fn silence_does_not_pass() {
        let silent = ToneMeasurement::silent(TONE_HZ, 48_000);
        assert!(!tone_present(&silent, &silent).ok);
    }

    #[test]
    fn music_already_playing_does_not_pass() {
        let music = m(0.004, 0.08);
        let verdict = tone_present(&music, &m(0.0045, 0.081));
        assert!(!verdict.ok, "{verdict:?}");
    }

    #[test]
    fn tone_over_music_passes() {
        let music = m(0.0005, 0.08);
        assert!(tone_present(&music, &m(0.021, 0.083)).ok);
    }

    #[test]
    fn tone_buried_under_loud_sound_is_rejected_by_the_ratio() {
        let silent = ToneMeasurement::silent(TONE_HZ, 48_000);
        let loud = m(0.021, 0.4);
        assert!(!tone_present(&silent, &loud).ok);
    }

    #[test]
    fn absence_is_judged_against_the_level_when_present() {
        let silent = ToneMeasurement::silent(TONE_HZ, 48_000);
        let on = m(0.021, 0.021);
        assert!(tone_absent(&silent, &on, &m(0.0003, 0.001)).ok);
        assert!(tone_absent(&silent, &on, &ToneMeasurement::silent(TONE_HZ, 48_000)).ok);
        assert!(!tone_absent(&silent, &on, &m(0.012, 0.012)).ok);
    }

    #[test]
    fn noisy_baseline_is_flagged() {
        assert!(baseline_is_noisy(&m(0.0, 0.05)));
        assert!(!baseline_is_noisy(&m(0.0, 0.001)));
    }
}
