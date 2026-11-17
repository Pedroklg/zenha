//! Análise de tom com filtro de Goertzel, sem dependência de plataforma.

use std::f64::consts::TAU;

use serde::{Deserialize, Serialize};

const MIN_SAMPLES: usize = 64;
const SEARCH_OFFSETS_IN_BINS: [f64; 5] = [-1.0, -0.5, 0.0, 0.5, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ToneMeasurement {
    pub hz: f64,
    pub sample_rate: u32,
    pub samples: usize,
    pub total_rms: f64,
    pub tone_rms: f64,
    pub tone_ratio: f64,
    pub peak: f64,
    pub coverage: f64,
}

impl ToneMeasurement {
    pub fn silent(hz: f64, sample_rate: u32) -> Self {
        Self {
            hz,
            sample_rate,
            samples: 0,
            total_rms: 0.0,
            tone_rms: 0.0,
            tone_ratio: 0.0,
            peak: 0.0,
            coverage: 0.0,
        }
    }
}

fn hann(len: usize) -> Vec<f64> {
    let denominator = (len - 1) as f64;
    (0..len)
        .map(|n| 0.5 - 0.5 * (TAU * n as f64 / denominator).cos())
        .collect()
}

/// Squared magnitude of the DFT of `data` at `hz`, for a fractional bin.
fn goertzel_power(data: &[f64], sample_rate: f64, hz: f64) -> f64 {
    let coefficient = 2.0 * (TAU * hz / sample_rate).cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &value in data {
        let s0 = value + coefficient * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coefficient * s1 * s2).max(0.0)
}

/// Power of `samples` (mono, -1.0 to 1.0) at `hz` against the total power.
///
/// A Hann window is applied. The tone is searched within one bin around `hz` to absorb
/// the clock difference between the sender and the capture device.
pub fn analyze_tone(samples: &[f32], sample_rate: u32, hz: f64) -> ToneMeasurement {
    if samples.len() < MIN_SAMPLES || sample_rate == 0 {
        return ToneMeasurement::silent(hz, sample_rate);
    }
    let rate = f64::from(sample_rate);
    let window = hann(samples.len());
    let weighted: Vec<f64> = samples
        .iter()
        .zip(&window)
        .map(|(&sample, &w)| f64::from(sample) * w)
        .collect();
    let window_sum: f64 = window.iter().sum();
    let window_energy: f64 = window.iter().map(|w| w * w).sum();

    let plain_energy: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    let total_rms = (plain_energy / samples.len() as f64).sqrt();
    let peak = samples
        .iter()
        .map(|&s| f64::from(s).abs())
        .fold(0.0, f64::max);

    let bin_hz = rate / samples.len() as f64;
    let strongest = SEARCH_OFFSETS_IN_BINS
        .iter()
        .map(|offset| goertzel_power(&weighted, rate, hz + offset * bin_hz))
        .fold(0.0, f64::max);
    let tone_mean_square = 2.0 * strongest / (window_sum * window_sum);
    let weighted_mean_square = weighted.iter().map(|v| v * v).sum::<f64>() / window_energy;
    let tone_ratio = if weighted_mean_square > 1e-18 {
        (tone_mean_square / weighted_mean_square).min(1.0)
    } else {
        0.0
    };
    ToneMeasurement {
        hz,
        sample_rate,
        samples: samples.len(),
        total_rms,
        tone_rms: tone_mean_square.sqrt(),
        tone_ratio,
        peak,
        coverage: 1.0,
    }
}

/// Averages interleaved frames of `channels` samples into one mono signal.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(hz: f64, amplitude: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let count = (f64::from(rate) * seconds) as usize;
        (0..count)
            .map(|n| (amplitude * (TAU * hz * n as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    struct Lcg(u64);

    impl Lcg {
        fn next_unit(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        }
    }

    fn noise(amplitude: f64, count: usize, seed: u64) -> Vec<f32> {
        let mut rng = Lcg(seed);
        (0..count)
            .map(|_| (amplitude * rng.next_unit()) as f32)
            .collect()
    }

    fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(x, y)| x + y).collect()
    }

    #[test]
    fn pure_tone_is_measured_with_its_amplitude() {
        let m = analyze_tone(&sine(1290.0, 0.03, 0.5, RATE), RATE, 1290.0);
        assert!((m.tone_rms - 0.03 / 2f64.sqrt()).abs() < 0.0015, "{m:?}");
        assert!(m.tone_ratio > 0.95, "{m:?}");
        assert!((m.total_rms - 0.0212).abs() < 0.001, "{m:?}");
        assert!((m.peak - 0.03).abs() < 0.001);
        assert_eq!(m.samples, 24_000);
    }

    #[test]
    fn tone_is_found_in_noise() {
        let signal = mix(
            &sine(1290.0, 0.03, 1.0, RATE),
            &noise(0.1, RATE as usize, 7),
        );
        let m = analyze_tone(&signal, RATE, 1290.0);
        assert!((m.tone_rms - 0.0212).abs() < 0.004, "{m:?}");
        assert!(m.tone_ratio > 0.03 && m.tone_ratio < 0.2, "{m:?}");
        let noise_only = analyze_tone(&noise(0.1, RATE as usize, 7), RATE, 1290.0);
        assert!(
            m.tone_rms > 5.0 * noise_only.tone_rms,
            "{m:?} {noise_only:?}"
        );
    }

    #[test]
    fn silence_and_empty_input_have_no_tone() {
        let m = analyze_tone(&vec![0.0; 24_000], RATE, 1290.0);
        assert_eq!(
            (m.tone_rms, m.tone_ratio, m.total_rms, m.peak),
            (0.0, 0.0, 0.0, 0.0)
        );
        let empty = analyze_tone(&[], RATE, 1290.0);
        assert_eq!(empty.samples, 0);
        assert_eq!(empty.coverage, 0.0);
    }

    #[test]
    fn other_frequencies_are_rejected() {
        for hz in [440.0, 997.0, 1500.0, 2580.0, 3000.0] {
            let m = analyze_tone(&sine(hz, 0.05, 0.5, RATE), RATE, 1290.0);
            assert!(m.tone_ratio < 0.01, "{hz}: {m:?}");
            assert!(m.tone_rms < 0.0005, "{hz}: {m:?}");
        }
    }

    #[test]
    fn small_frequency_offsets_are_tolerated() {
        for hz in [1288.5, 1291.5] {
            let m = analyze_tone(&sine(hz, 0.03, 0.5, RATE), RATE, 1290.0);
            assert!(m.tone_rms > 0.015, "{hz}: {m:?}");
        }
    }

    #[test]
    fn works_at_44100_hz() {
        let m = analyze_tone(&sine(1290.0, 0.03, 0.5, 44_100), 44_100, 1290.0);
        assert!(m.tone_ratio > 0.95, "{m:?}");
    }

    #[test]
    fn partial_tone_is_still_detected_with_less_power() {
        let mut signal = vec![0.0f32; 12_000];
        signal.extend(sine(1290.0, 0.03, 0.25, RATE));
        let m = analyze_tone(&signal, RATE, 1290.0);
        assert!(m.tone_rms > 0.005 && m.tone_rms < 0.0212, "{m:?}");
    }

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[1.0, 2.0], 1), vec![1.0, 2.0]);
        assert_eq!(downmix(&[1.0, 2.0, 3.0], 2), vec![1.5]);
    }
}
