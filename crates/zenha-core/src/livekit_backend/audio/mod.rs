//! LiveKit backend audio: devices, microphone, test tone and screen audio.

mod devices;
mod microphone;
mod share;

use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use futures_util::StreamExt;
use livekit::prelude::{AudioError, PlatformAudio, TrackSource};
use livekit::webrtc::audio_stream::native::NativeAudioStream;

use crate::api::{AudioDevices, NoiseSuppression, SessionError};
use crate::audio::mic_capture::MicCapture;
use crate::audio::participant_volume::volume_gain;
use crate::livekit_backend::room::{
    apply_microphone_mute, apply_participant_volume, attach_voices, blocking,
};
use crate::livekit_backend::{LiveKitBackend, TaskGuard};

use devices::{apply_input, apply_output, input_devices, output_devices, processing_for};
pub(super) use devices::{apply_selection, engine_output_opens, open_audio, stop_engine_output};
pub(super) use microphone::{LocalSpeaker, publish_selected_microphone, publish_tone};
pub(super) use share::{ShareAudio, publish_share_audio, stop_share_audio};

#[derive(Debug, Clone, Default)]
pub(super) struct Selection {
    pub(super) input: Option<String>,
    pub(super) output: Option<String>,
    pub(super) noise: NoiseSuppression,
}

/// Keeps alive whatever feeds the published microphone track.
pub(super) enum Microphone {
    None,
    Platform {
        _speech: TaskGuard,
    },
    Enhanced {
        _mic: EnhancedMic,
    },
    Tone {
        _task: TaskGuard,
        _speech: TaskGuard,
    },
}

pub(super) struct EnhancedMic {
    _pump: TaskGuard,
    _capture: MicCapture,
}

impl LiveKitBackend {
    pub(super) fn selection(&self) -> Selection {
        self.selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update_selection<T>(&self, change: impl FnOnce(&mut Selection) -> T) -> T {
        change(
            &mut self
                .selection
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    async fn run_audio<T: Send + 'static>(
        &self,
        work: impl FnOnce(&PlatformAudio) -> T + Send + 'static,
    ) -> Result<T, SessionError> {
        let shared = self.current_audio().await;
        blocking(move || -> Result<T, AudioError> {
            let audio = match shared {
                Some(audio) => audio,
                None => PlatformAudio::new()?,
            };
            Ok(work(&audio))
        })
        .await?
        .map_err(|err| SessionError::Internal(format!("platform audio unavailable: {err}")))
    }

    /// Swaps the published microphone for one that matches the current selection.
    pub(in crate::livekit_backend) async fn republish_microphone(&self) {
        let selection = self.selection();
        let mut slot = self.connection.lock().await;
        let Some(connection) = slot.as_mut() else {
            return;
        };
        let Some(audio) = connection.audio.clone() else {
            return;
        };
        if matches!(connection.mic, Microphone::Tone { .. }) {
            return;
        }
        connection.mic = Microphone::None;
        let local = connection.room.local_participant();
        for (sid, publication) in local.track_publications() {
            if publication.source() != TrackSource::Microphone {
                continue;
            }
            if let Err(err) = local.unpublish_track(&sid).await {
                tracing::warn!(%err, "could not unpublish the microphone");
            }
        }
        if let Err(err) = audio.configure_audio_processing(processing_for(selection.noise)) {
            tracing::warn!(%err, "could not configure audio processing");
        }
        if !selection.noise.uses_own_capture() {
            let device = audio.clone();
            let input = selection.input.clone();
            let applied = blocking(move || apply_input(&device, input.as_deref(), false)).await;
            if let Ok(Err(err)) | Err(err) = applied.map(|inner| inner.map(|_| ())) {
                tracing::warn!(%err, "could not select the input device");
            }
        }
        connection.mic = publish_selected_microphone(
            &connection.room,
            &audio,
            &selection,
            &connection.events_tx,
            &self.flags,
        )
        .await;
        if self.flags.microphone_muted.load(Ordering::SeqCst) {
            apply_microphone_mute(&connection.room, true);
        }
    }

    pub(super) async fn list_audio_devices(&self) -> Result<AudioDevices, SessionError> {
        let selection = self.selection();
        if self.options.is_tone() {
            return Ok(AudioDevices {
                selected_input: selection.input,
                selected_output: selection.output,
                ..AudioDevices::default()
            });
        }
        let listing = self
            .run_audio(|audio| {
                (
                    audio.recording_devices().collect::<Vec<_>>(),
                    audio.playout_devices().collect::<Vec<_>>(),
                )
            })
            .await;
        let (inputs, outputs) = listing.unwrap_or_else(|err| {
            tracing::warn!(%err, "could not list audio devices");
            (Vec::new(), Vec::new())
        });
        Ok(AudioDevices {
            inputs: input_devices(&inputs),
            outputs: output_devices(&outputs),
            selected_input: selection.input,
            selected_output: selection.output,
        })
    }

    pub(super) async fn select_input_device(&self, id: Option<String>) -> Result<(), SessionError> {
        let previous = self.update_selection(|s| std::mem::replace(&mut s.input, id.clone()));
        let Some(audio) = self.current_audio().await else {
            return Ok(());
        };
        if self.selection().noise.uses_own_capture() {
            // The enhanced path captures by itself, so its capture restarts on the new device.
            self.republish_microphone().await;
            return Ok(());
        }
        let result = blocking(move || apply_input(&audio, id.as_deref(), true)).await;
        match result.and_then(|applied| applied.map(|_| ())) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.update_selection(|s| s.input = previous);
                Err(err)
            }
        }
    }

    pub(super) async fn select_output_device(
        &self,
        id: Option<String>,
    ) -> Result<(), SessionError> {
        let previous = self.update_selection(|s| std::mem::replace(&mut s.output, id.clone()));
        self.flags.set_stream_output(id.clone());
        let Some(audio) = self.current_audio().await else {
            return Ok(());
        };
        if self.flags.own_voice_output.load(Ordering::SeqCst) {
            return Ok(());
        }
        let result = blocking(move || apply_output(&audio, id.as_deref(), true)).await;
        match result.and_then(|applied| applied.map(|_| ())) {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::warn!(%err, "output device switch failed");
                let Some(room) = self.current_room().await else {
                    self.update_selection(|s| s.output = previous);
                    return Err(err);
                };
                blocking(stop_engine_output).await?;
                self.flags.own_voice_output.store(true, Ordering::SeqCst);
                attach_voices(&room, &self.flags);
                Ok(())
            }
        }
    }

    pub(super) async fn select_noise_suppression(
        &self,
        mode: NoiseSuppression,
    ) -> Result<(), SessionError> {
        let previous = self.update_selection(|s| std::mem::replace(&mut s.noise, mode));
        if previous != mode {
            self.republish_microphone().await;
        }
        Ok(())
    }

    pub(super) async fn update_participant_volume(
        &self,
        identity: &str,
        percent: u16,
    ) -> Result<(), SessionError> {
        let clamped = self.flags.set_participant_volume(identity, percent);
        let gain = volume_gain(clamped);
        self.flags.set_voice_gain(identity, gain as f32);
        let applied_to_tracks = if let Some(room) = self.current_room().await {
            apply_participant_volume(&room, identity, gain)
        } else {
            0
        };
        tracing::debug!(
            identity,
            percent = clamped,
            gain,
            applied_to_tracks,
            "participant volume updated"
        );
        Ok(())
    }
}

pub(super) async fn audio_tap(identity: String, kind: &'static str, mut stream: NativeAudioStream) {
    let mut samples = 0u64;
    let mut energy = 0f64;
    let mut crossings = 0u64;
    let mut previous = 0i16;
    while let Some(frame) = stream.next().await {
        for &sample in frame.data.iter() {
            energy += f64::from(sample) * f64::from(sample);
            if (previous < 0) != (sample < 0) {
                crossings += 1;
            }
            previous = sample;
        }
        samples += frame.data.len() as u64;
        if samples >= 48_000 {
            let rms = (energy / samples as f64).sqrt() / 32768.0;
            let hz = crossings as f64 * 24_000.0 / samples as f64;
            tracing::info!(%identity, kind, rms, hz, "received audio tap");
            samples = 0;
            energy = 0.0;
            crossings = 0;
        }
    }
    tracing::info!(%identity, "received audio tap ended");
}

#[cfg(test)]
mod tests {
    use livekit::webrtc::{
        audio_source::{AudioSourceOptions, native::NativeAudioSource},
        peer_connection_factory::{PeerConnectionFactory, native::PeerConnectionFactoryExt},
    };

    use crate::backend::MediaBackend;

    use super::super::LiveKitBackend;

    #[test]
    fn audio_track_set_volume_accepts_in_and_out_of_range_values() {
        let factory = PeerConnectionFactory::default();
        let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
        let track = factory.create_audio_track("test-volume", source);
        track.set_volume(0.5);
        track.set_volume(20.0);
    }

    #[tokio::test]
    async fn participant_volume_flags_store_clamp_and_apply_to_audio_track() {
        let backend = LiveKitBackend::new();
        backend.set_participant_volume("alice", 250).await.unwrap();
        backend.set_participant_volume("bob", 100).await.unwrap();

        let factory = PeerConnectionFactory::default();
        let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
        let track = factory.create_audio_track("test-participant-volume", source);
        backend.flags.apply_participant_volume("alice", &track);
        backend.flags.apply_participant_volume("bob", &track);

        let guard = backend.flags.participant_volumes.lock().unwrap();
        assert_eq!(guard.gain_for("alice"), Some(2.0));
        assert_eq!(guard.gain_for("bob"), None);
    }
}
