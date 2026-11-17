//! DeepFilterNet 3 engine (tract, Windows only) behind the `Denoiser` trait.

use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;

use crate::audio::mic_capture::CAPTURE_SAMPLE_RATE;
use crate::audio::noise::{DENOISE_FRAME, Denoiser, speech_from_lsnr};

pub(crate) struct DeepFilterDenoiser {
    model: DfTract,
    input: Array2<f32>,
    output: Array2<f32>,
}

impl DeepFilterDenoiser {
    pub(crate) fn new() -> Result<Self, String> {
        let model = DfTract::new(DfParams::default(), &RuntimeParams::default_with_ch(1))
            .map_err(|err| format!("{err:#}"))?;
        if model.sr != CAPTURE_SAMPLE_RATE as usize || model.hop_size != DENOISE_FRAME {
            return Err(format!(
                "unsupported model format: {} Hz, hop {}",
                model.sr, model.hop_size
            ));
        }
        Ok(Self {
            model,
            input: Array2::zeros((1, DENOISE_FRAME)),
            output: Array2::zeros((1, DENOISE_FRAME)),
        })
    }
}

impl Denoiser for DeepFilterDenoiser {
    fn process(&mut self, frame: &mut [i16]) -> Option<f32> {
        if frame.len() != DENOISE_FRAME {
            return None;
        }
        for (slot, sample) in self.input.iter_mut().zip(frame.iter()) {
            *slot = f32::from(*sample) / 32768.0;
        }
        let lsnr = match self
            .model
            .process(self.input.view(), self.output.view_mut())
        {
            Ok(lsnr) => lsnr,
            Err(err) => {
                tracing::debug!(%err, "deepfilternet failed for a frame");
                return None;
            }
        };
        for (sample, value) in frame.iter_mut().zip(self.output.iter()) {
            *sample = (value * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        }
        Some(speech_from_lsnr(lsnr))
    }
}

#[cfg(test)]
mod tests {
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
        let mut denoiser = DeepFilterDenoiser::new().expect("model loads");
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
        let mut denoiser = DeepFilterDenoiser::new().expect("model loads");
        let mut frame = vec![123i16; 100];
        assert_eq!(denoiser.process(&mut frame), None);
        assert!(frame.iter().all(|s| *s == 123));
    }
}
