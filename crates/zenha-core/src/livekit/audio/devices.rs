//! Listagem e seleção de dispositivos de áudio da plataforma.

use livekit::prelude::{
    AudioProcessingOptions, PlatformAudio, PlayoutDeviceInfo, RecordingDeviceInfo,
};

use crate::api::{AudioDevice, NoiseSuppression, SessionError};
use crate::backend::support::{audio_device, unique_devices};

use super::Selection;

pub(super) fn input_devices(infos: &[RecordingDeviceInfo]) -> Vec<AudioDevice> {
    unique_devices(
        infos
            .iter()
            .map(|info| audio_device(info.id.as_str(), &info.name))
            .collect(),
    )
}

pub(super) fn output_devices(infos: &[PlayoutDeviceInfo]) -> Vec<AudioDevice> {
    unique_devices(
        infos
            .iter()
            .map(|info| audio_device(info.id.as_str(), &info.name))
            .collect(),
    )
}

/// `running` selects the hot-swap call, which stops and restarts active audio;
/// before the room starts the plain setter is the right one.
pub(super) fn apply_input(
    audio: &PlatformAudio,
    requested: Option<&str>,
    running: bool,
) -> Result<Option<String>, SessionError> {
    let fallback;
    let requested = match requested {
        Some(requested) => requested,
        None => match crate::audio::default_device::default_input_id() {
            Some(id) => {
                fallback = id;
                fallback.as_str()
            }
            None => {
                tracing::warn!("could not resolve the Windows default input device");
                return Ok(None);
            }
        },
    };
    let Some(info) = audio
        .recording_devices()
        .find(|device| device.id.as_str() == requested)
    else {
        tracing::warn!(
            device_id = requested,
            "input device not found, leaving the engine default"
        );
        return Ok(None);
    };
    let outcome = if running {
        audio.switch_recording_device(&info.id)
    } else {
        audio.set_recording_device(&info.id)
    };
    outcome.map_err(|err| {
        SessionError::Internal(format!("could not select the input device: {err}"))
    })?;
    Ok(Some(info.name))
}

pub(super) fn apply_output(
    audio: &PlatformAudio,
    requested: Option<&str>,
    running: bool,
) -> Result<Option<String>, SessionError> {
    let fallback;
    let requested = match requested {
        Some(requested) => requested,
        None => match crate::audio::default_device::default_output_id() {
            Some(id) => {
                fallback = id;
                fallback.as_str()
            }
            None => {
                tracing::warn!("could not resolve the Windows default output device");
                return Ok(None);
            }
        },
    };
    let Some(info) = audio
        .playout_devices()
        .find(|device| device.id.as_str() == requested)
    else {
        tracing::warn!(
            device_id = requested,
            "output device not found, leaving the engine default"
        );
        return Ok(None);
    };
    let outcome = if running {
        audio.switch_playout_device(&info.id)
    } else {
        audio.set_playout_device(&info.id)
    };
    outcome.map_err(|err| {
        SessionError::Internal(format!("could not select the output device: {err}"))
    })?;
    Ok(Some(info.name))
}

pub(in crate::livekit) fn apply_selection(audio: &PlatformAudio, selection: &Selection) {
    for device in audio.recording_devices() {
        tracing::info!(index = device.index, id = %device.id, name = %device.name, "audio input device available");
    }
    for device in audio.playout_devices() {
        tracing::info!(index = device.index, id = %device.id, name = %device.name, "audio output device available");
    }
    let input = apply_input(audio, selection.input.as_deref(), false).unwrap_or_else(|err| {
        tracing::warn!(%err, "using the system default input device");
        None
    });
    let output = apply_output(audio, selection.output.as_deref(), false).unwrap_or_else(|err| {
        tracing::warn!(%err, "using the system default output device");
        None
    });
    tracing::info!(
        input = input.as_deref().unwrap_or("engine default"),
        output = output.as_deref().unwrap_or("engine default"),
        input_is_system_default = selection.input.is_none(),
        output_is_system_default = selection.output.is_none(),
        "audio devices chosen for this connection"
    );
}

pub(super) fn processing_for(mode: NoiseSuppression) -> AudioProcessingOptions {
    AudioProcessingOptions {
        noise_suppression: mode == NoiseSuppression::Standard,
        ..AudioProcessingOptions::default()
    }
}

pub(in crate::livekit) fn open_audio(mode: NoiseSuppression) -> Option<PlatformAudio> {
    match PlatformAudio::new() {
        Ok(audio) => {
            if let Err(err) = audio.configure_audio_processing(processing_for(mode)) {
                tracing::warn!(%err, "could not configure audio processing");
            }
            Some(audio)
        }
        Err(err) => {
            tracing::warn!(%err, "platform audio unavailable, no microphone and no playback");
            None
        }
    }
}
