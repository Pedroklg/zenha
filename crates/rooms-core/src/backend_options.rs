//! Opções de teste do backend do LiveKit. Os valores padrão reproduzem o comportamento do cliente.

use std::f64::consts::TAU;

use crate::api::SessionError;
use crate::video::{CaptureKind, CaptureTarget};

pub const TEST_PATTERN_TARGET_ID: &str = "test:pattern";
pub const TEST_PATTERN_TARGET_NAME: &str = "Padrão de teste";
pub const TEST_PATTERN_SIZE: (u32, u32) = (640, 360);
pub const TONE_SAMPLE_RATE: u32 = 48_000;
pub const TONE_FRAME_SAMPLES: usize = 480;

const MIN_TONE_HZ: f64 = 50.0;
const MAX_TONE_HZ: f64 = 20_000.0;

/// De onde vem o áudio publicado como microfone.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum MicrophoneSource {
    /// Microfone real pelo áudio da plataforma (comportamento do cliente).
    #[default]
    Device,
    /// Senoide sintética; nenhum áudio de plataforma é criado, então não grava nem reproduz.
    Tone { hz: f64, amplitude: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LiveKitBackendOptions {
    pub microphone: MicrophoneSource,
    /// Lista e aceita o alvo de captura "Padrão de teste" (barras geradas em Rust).
    pub test_pattern_target: bool,
}

impl LiveKitBackendOptions {
    pub fn validate(&self) -> Result<(), SessionError> {
        if let MicrophoneSource::Tone { hz, amplitude } = self.microphone {
            let nyquist = f64::from(TONE_SAMPLE_RATE) / 2.0;
            if !hz.is_finite() || !(MIN_TONE_HZ..=MAX_TONE_HZ.min(nyquist)).contains(&hz) {
                return Err(SessionError::InvalidInput(format!(
                    "tone frequency must be between {MIN_TONE_HZ} and {MAX_TONE_HZ} Hz"
                )));
            }
            if !amplitude.is_finite() || amplitude <= 0.0 || amplitude > 1.0 {
                return Err(SessionError::InvalidInput(
                    "tone amplitude must be in (0, 1]".to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub fn is_tone(&self) -> bool {
        matches!(self.microphone, MicrophoneSource::Tone { .. })
    }
}

pub fn test_pattern_target() -> CaptureTarget {
    CaptureTarget {
        id: TEST_PATTERN_TARGET_ID.to_owned(),
        name: TEST_PATTERN_TARGET_NAME.to_owned(),
        kind: CaptureKind::Screen,
    }
}

/// Gerador de senoide em PCM de 16 bits, contínuo entre chamadas.
#[derive(Debug, Clone)]
pub struct ToneGenerator {
    phase: f64,
    step: f64,
    amplitude: f64,
}

impl ToneGenerator {
    pub fn new(hz: f64, amplitude: f64, sample_rate: u32) -> Self {
        Self {
            phase: 0.0,
            step: TAU * hz / f64::from(sample_rate.max(1)),
            amplitude,
        }
    }

    pub fn fill(&mut self, out: &mut [i16]) {
        for sample in out {
            let value = (self.amplitude * self.phase.sin() * 32767.0).round();
            *sample = value.clamp(-32768.0, 32767.0) as i16;
            self.phase += self.step;
            if self.phase >= TAU {
                self.phase -= TAU;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_normal_client() {
        let options = LiveKitBackendOptions::default();
        assert_eq!(options.microphone, MicrophoneSource::Device);
        assert!(!options.test_pattern_target);
        assert!(!options.is_tone());
        assert!(options.validate().is_ok());
    }

    #[test]
    fn tone_parameters_are_validated() {
        let tone = |hz, amplitude| LiveKitBackendOptions {
            microphone: MicrophoneSource::Tone { hz, amplitude },
            test_pattern_target: false,
        };
        assert!(tone(1290.0, 0.03).validate().is_ok());
        assert!(tone(1290.0, 0.03).is_tone());
        for (hz, amplitude) in [
            (0.0, 0.1),
            (10.0, 0.1),
            (30_000.0, 0.1),
            (f64::NAN, 0.1),
            (1000.0, 0.0),
            (1000.0, 1.5),
            (1000.0, f64::NAN),
        ] {
            assert!(tone(hz, amplitude).validate().is_err(), "{hz} {amplitude}");
        }
    }

    #[test]
    fn generator_has_the_requested_amplitude_and_period() {
        let mut generator = ToneGenerator::new(1000.0, 0.5, 48_000);
        let mut samples = vec![0i16; 4800];
        generator.fill(&mut samples);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!((16_000..=16_500).contains(&peak), "peak {peak}");
        let crossings = samples
            .windows(2)
            .filter(|pair| pair[0] < 0 && pair[1] >= 0)
            .count();
        assert!((99..=101).contains(&crossings), "crossings {crossings}");
    }

    #[test]
    fn generator_is_continuous_across_calls() {
        let mut split = ToneGenerator::new(1290.0, 0.1, 48_000);
        let mut whole = ToneGenerator::new(1290.0, 0.1, 48_000);
        let mut a = vec![0i16; 480];
        let mut b = vec![0i16; 480];
        split.fill(&mut a[..200]);
        split.fill(&mut a[200..]);
        whole.fill(&mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn test_pattern_target_is_a_screen() {
        let target = test_pattern_target();
        assert_eq!(target.id, TEST_PATTERN_TARGET_ID);
        assert_eq!(target.kind, CaptureKind::Screen);
    }
}
