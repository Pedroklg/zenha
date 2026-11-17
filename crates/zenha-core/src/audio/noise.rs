//! Neural network noise suppression, behind its own interface so the engine can be swapped.

use nnnoiseless::DenoiseState;

/// Samples per frame: 10 ms of mono audio at 48 kHz.
pub const DENOISE_FRAME: usize = nnnoiseless::FRAME_SIZE;

const SILENCE_DB: f32 = -100.0;
const HYSTERESIS_DB: f32 = 6.0;
const SPEECH_OPEN: f32 = 0.55;
const SPEECH_KEEP: f32 = 0.30;
const HOLD_FRAMES: u32 = 25;
const LOOKAHEAD_FRAMES: usize = 2;
const ATTACK_SAMPLES: f32 = 240.0;
const RELEASE_SAMPLES: f32 = 3840.0;

/// Maps a local SNR estimate in dB to a speech probability: -5 dB is 0.0, 15 dB is 1.0.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn speech_from_lsnr(lsnr_db: f32) -> f32 {
    ((lsnr_db + 5.0) / 20.0).clamp(0.0, 1.0)
}

pub trait Denoiser {
    /// Cleans one frame of `DENOISE_FRAME` samples in place and returns the speech
    /// probability (0.0 to 1.0) when the engine computes one.
    fn process(&mut self, frame: &mut [i16]) -> Option<f32>;
}

pub struct RnnoiseDenoiser {
    state: Box<DenoiseState<'static>>,
    input: [f32; DENOISE_FRAME],
    output: [f32; DENOISE_FRAME],
}

impl RnnoiseDenoiser {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input: [0.0; DENOISE_FRAME],
            output: [0.0; DENOISE_FRAME],
        }
    }
}

impl Default for RnnoiseDenoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser for RnnoiseDenoiser {
    fn process(&mut self, frame: &mut [i16]) -> Option<f32> {
        if frame.len() != DENOISE_FRAME {
            return None;
        }
        // RNNoise works on f32 samples in the i16 range.
        for (slot, sample) in self.input.iter_mut().zip(frame.iter()) {
            *slot = f32::from(*sample);
        }
        let speech = self.state.process_frame(&mut self.output, &self.input);
        for (sample, value) in frame.iter_mut().zip(self.output.iter()) {
            *sample = value.round().clamp(-32768.0, 32767.0) as i16;
        }
        Some(speech.clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateReading {
    pub level_db: f32,
    pub open: bool,
}

/// Noise gate with hysteresis, hold and a short lookahead, driven by level and speech probability.
pub struct NoiseGate {
    open: bool,
    hold: u32,
    gain: f32,
    delay: [[i16; DENOISE_FRAME]; LOOKAHEAD_FRAMES],
    head: usize,
}

impl NoiseGate {
    pub fn new() -> Self {
        Self {
            open: false,
            hold: 0,
            gain: 0.0,
            delay: [[0; DENOISE_FRAME]; LOOKAHEAD_FRAMES],
            head: 0,
        }
    }

    fn reset(&mut self) {
        self.open = false;
        self.hold = 0;
        self.gain = 0.0;
        self.delay = [[0; DENOISE_FRAME]; LOOKAHEAD_FRAMES];
        self.head = 0;
    }

    /// `threshold_db` of `None` turns the gate off: audio passes untouched and undelayed.
    pub fn process(
        &mut self,
        frame: &mut [i16],
        threshold_db: Option<f32>,
        speech: Option<f32>,
    ) -> GateReading {
        if frame.len() != DENOISE_FRAME {
            return GateReading {
                level_db: SILENCE_DB,
                open: true,
            };
        }
        let level_db = level_db(frame);
        let Some(threshold) = threshold_db else {
            self.reset();
            return GateReading {
                level_db,
                open: true,
            };
        };

        let (level_floor, speech_floor) = if self.open {
            (threshold - HYSTERESIS_DB, SPEECH_KEEP)
        } else {
            (threshold, SPEECH_OPEN)
        };
        let present = level_db >= level_floor && speech.is_none_or(|p| p >= speech_floor);
        if present {
            self.open = true;
            self.hold = HOLD_FRAMES;
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.open = false;
        }

        let (target, step) = if self.open {
            (1.0, 1.0 / ATTACK_SAMPLES)
        } else {
            (0.0, -1.0 / RELEASE_SAMPLES)
        };
        let slot = &mut self.delay[self.head];
        for (sample, old) in frame.iter_mut().zip(slot.iter_mut()) {
            std::mem::swap(sample, old);
            self.gain = if step > 0.0 {
                (self.gain + step).min(target)
            } else {
                (self.gain + step).max(target)
            };
            *sample = (f32::from(*sample) * self.gain).round() as i16;
        }
        self.head = (self.head + 1) % LOOKAHEAD_FRAMES;

        GateReading {
            level_db,
            open: self.open,
        }
    }
}

impl Default for NoiseGate {
    fn default() -> Self {
        Self::new()
    }
}

fn level_db(frame: &[i16]) -> f32 {
    let energy: f32 = frame.iter().map(|s| f32::from(*s).powi(2)).sum();
    let rms = (energy / frame.len() as f32).sqrt();
    if rms <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * (rms / 32768.0).log10()).max(SILENCE_DB)
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use super::*;

    fn rms(samples: &[i16]) -> f64 {
        let energy: f64 = samples.iter().map(|s| f64::from(*s).powi(2)).sum();
        (energy / samples.len() as f64).sqrt()
    }

    /// Deterministic white noise, about -30 dBFS.
    fn noise(len: usize) -> Vec<i16> {
        let mut seed = 0x2545_f491u32;
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                ((seed % 2001) as i16) - 1000
            })
            .collect()
    }

    #[test]
    fn steady_noise_is_strongly_reduced() {
        let mut denoiser = RnnoiseDenoiser::new();
        let mut audio = noise(DENOISE_FRAME * 200);
        let before = rms(&audio[DENOISE_FRAME * 100..]);
        for frame in audio.chunks_mut(DENOISE_FRAME) {
            denoiser.process(frame);
        }
        let after = rms(&audio[DENOISE_FRAME * 100..]);
        assert!(after < before / 4.0, "before {before:.1}, after {after:.1}");
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_left_untouched() {
        let mut denoiser = RnnoiseDenoiser::new();
        let mut frame = vec![123i16; 100];
        assert_eq!(denoiser.process(&mut frame), None);
        assert!(frame.iter().all(|s| *s == 123));
    }

    #[test]
    fn a_full_frame_reports_a_speech_probability() {
        let mut denoiser = RnnoiseDenoiser::new();
        let mut frame = noise(DENOISE_FRAME);
        let speech = denoiser.process(&mut frame);
        assert!(matches!(speech, Some(p) if (0.0..=1.0).contains(&p)));
    }

    #[test]
    fn lsnr_maps_linearly_to_a_clamped_probability() {
        assert_eq!(speech_from_lsnr(-15.0), 0.0);
        assert_eq!(speech_from_lsnr(-5.0), 0.0);
        assert!((speech_from_lsnr(5.0) - 0.5).abs() < 1e-6);
        assert_eq!(speech_from_lsnr(15.0), 1.0);
        assert_eq!(speech_from_lsnr(40.0), 1.0);
    }

    const N: usize = DENOISE_FRAME;
    const THRESHOLD: f32 = -30.0;

    fn sine(frames: usize, amplitude: f32) -> Vec<i16> {
        (0..frames * N)
            .map(|i| (amplitude * (2.0 * PI * 4.0 * i as f32 / N as f32).sin()).round() as i16)
            .collect()
    }

    fn run(
        gate: &mut NoiseGate,
        audio: &mut [i16],
        threshold: Option<f32>,
        speech: Option<f32>,
    ) -> Vec<GateReading> {
        audio
            .chunks_mut(N)
            .map(|frame| gate.process(frame, threshold, speech))
            .collect()
    }

    #[test]
    fn disabled_gate_passes_audio_unchanged_and_undelayed() {
        let mut gate = NoiseGate::new();
        let original = sine(5, 3000.0);
        let mut audio = original.clone();
        let readings = run(&mut gate, &mut audio, None, Some(0.0));
        assert_eq!(audio, original);
        assert!(readings.iter().all(|r| r.open));
    }

    #[test]
    fn noise_below_the_threshold_is_silenced() {
        let mut gate = NoiseGate::new();
        let mut audio = noise(N * 50);
        let readings = run(&mut gate, &mut audio, Some(-20.0), Some(0.9));
        assert!(audio.iter().all(|s| *s == 0));
        assert!(readings.iter().all(|r| !r.open));
    }

    #[test]
    fn loud_speech_opens_and_output_is_delayed_by_the_lookahead() {
        let mut gate = NoiseGate::new();
        let original = sine(20, 10_000.0);
        let mut audio = original.clone();
        let readings = run(&mut gate, &mut audio, Some(THRESHOLD), Some(0.9));
        assert!(readings.iter().all(|r| r.open));
        let delay = LOOKAHEAD_FRAMES * N;
        assert!(audio[..delay].iter().all(|s| *s == 0));
        assert_eq!(audio[delay..], original[..original.len() - delay]);
    }

    #[test]
    fn loud_non_speech_does_not_open() {
        let mut gate = NoiseGate::new();
        let mut audio = sine(30, 10_000.0);
        let readings = run(&mut gate, &mut audio, Some(THRESHOLD), Some(0.1));
        assert!(readings.iter().all(|r| !r.open));
        assert!(audio.iter().all(|s| *s == 0));
    }

    #[test]
    fn without_a_speech_probability_only_the_level_decides() {
        let mut gate = NoiseGate::new();
        let mut loud = sine(5, 10_000.0);
        assert!(run(&mut gate, &mut loud, Some(THRESHOLD), None)[0].open);

        let mut gate = NoiseGate::new();
        let mut quiet = sine(5, 20.0);
        assert!(!run(&mut gate, &mut quiet, Some(THRESHOLD), None)[0].open);
    }

    #[test]
    fn gate_holds_then_closes_with_a_smooth_release() {
        let mut gate = NoiseGate::new();
        let mut audio = vec![8000i16; N * 80];
        let (voiced, rest) = audio.split_at_mut(N * 10);
        run(&mut gate, voiced, Some(THRESHOLD), Some(0.9));
        let readings = run(&mut gate, rest, Some(THRESHOLD), Some(0.0));
        let open_frames = readings.iter().take_while(|r| r.open).count();
        assert_eq!(open_frames, HOLD_FRAMES as usize);
        assert!(readings[open_frames..].iter().all(|r| !r.open));

        let gains: Vec<f32> = rest.iter().map(|s| f32::from(*s) / 8000.0).collect();
        let tail = &gains[(HOLD_FRAMES as usize + LOOKAHEAD_FRAMES) * N - 1..];
        assert!(tail.windows(2).all(|w| w[1] <= w[0]));
        let max_step = tail.windows(2).map(|w| w[0] - w[1]).fold(0.0, f32::max);
        assert!(max_step <= 1.0 / RELEASE_SAMPLES + 0.001, "step {max_step}");
        assert_eq!(*gains.last().unwrap_or(&1.0), 0.0);
    }

    #[test]
    fn hysteresis_keeps_an_open_gate_open_but_does_not_open_a_closed_one() {
        let between = 732i16;
        let mut gate = NoiseGate::new();
        let mut closed = vec![between; N * 40];
        assert!(
            run(&mut gate, &mut closed, Some(THRESHOLD), None)
                .iter()
                .all(|r| !r.open)
        );

        let mut gate = NoiseGate::new();
        let mut loud = vec![8000i16; N * 2];
        run(&mut gate, &mut loud, Some(THRESHOLD), None);
        let mut held = vec![between; N * 80];
        assert!(
            run(&mut gate, &mut held, Some(THRESHOLD), None)
                .iter()
                .all(|r| r.open)
        );
    }

    #[test]
    fn level_is_reported_in_dbfs() {
        let mut gate = NoiseGate::new();
        let mut audio = sine(1, 10_000.0);
        let reading = gate.process(&mut audio, Some(THRESHOLD), None);
        let expected = 20.0 * (10_000.0f32 / 2f32.sqrt() / 32768.0).log10();
        assert!(
            (reading.level_db - expected).abs() < 0.5,
            "{}",
            reading.level_db
        );

        let mut zeros = vec![0i16; N];
        assert_eq!(
            gate.process(&mut zeros, Some(THRESHOLD), None).level_db,
            -100.0
        );
    }

    #[test]
    fn gate_ignores_frames_of_the_wrong_size() {
        let mut gate = NoiseGate::new();
        let mut frame = vec![5i16; 100];
        let reading = gate.process(&mut frame, Some(THRESHOLD), Some(0.0));
        assert_eq!(
            reading,
            GateReading {
                level_db: -100.0,
                open: true
            }
        );
        assert!(frame.iter().all(|s| *s == 5));
    }
}
