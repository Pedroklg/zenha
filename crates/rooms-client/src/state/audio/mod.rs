//! Estado de áudio, dispositivos, volumes por participante, atalhos e push-to-talk.

mod devices;
mod hotkeys;
mod noise;
mod ptt;
mod voice;
mod volumes;

use std::collections::BTreeMap;

use crate::settings::{NoiseMode, Settings, VoiceMode};
use crate::ui_model::AudioDeviceView;

use devices::selected_label;

pub use devices::DEFAULT_DEVICE_LABEL;
pub use hotkeys::HotkeyState;
pub use noise::MicLevelView;
pub use voice::LobbyVoiceState;
pub(super) use voice::{toggle_deafen_fields, toggle_mute_fields};
pub use volumes::{
    DEFAULT_PARTICIPANT_VOLUME, FULL_VOLUME, MAX_PARTICIPANT_VOLUME, normalize_participant_volumes,
    participant_volume_for, saved_volume_commands, update_participant_volume,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioState {
    pub open: bool,
    pub inputs: Vec<AudioDeviceView>,
    pub outputs: Vec<AudioDeviceView>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub noise: NoiseMode,
    pub voice_mode: VoiceMode,
    pub stream_volume: u8,
    pub participant_volumes: BTreeMap<String, u16>,
    pub noise_gate: Option<i8>,
    pub mic_level: Option<MicLevelView>,
    pub(super) loaded: bool,
}

impl AudioState {
    pub(super) fn from_settings(settings: &Settings) -> Self {
        Self {
            input: settings.input_device.clone(),
            output: settings.output_device.clone(),
            noise: settings.noise_suppression,
            voice_mode: settings.voice_mode,
            stream_volume: settings
                .stream_volume
                .unwrap_or(FULL_VOLUME)
                .min(FULL_VOLUME),
            participant_volumes: normalize_participant_volumes(&settings.participant_volumes),
            noise_gate: settings.noise_gate_db,
            ..Self::default()
        }
    }

    pub fn input_label(&self) -> String {
        selected_label(&self.inputs, self.input.as_deref(), self.loaded)
    }

    pub fn output_label(&self) -> String {
        selected_label(&self.outputs, self.output.as_deref(), self.loaded)
    }
}
