//! Audio: microphone capture, noise suppression, playback, sound cues and volumes.

#[cfg(windows)]
pub(crate) mod audio_render;
pub(crate) mod cue;
#[cfg(windows)]
pub(crate) mod default_device;
#[cfg(windows)]
pub(crate) mod mic_capture;
pub(crate) mod noise;
pub(crate) mod participant_volume;
#[cfg(windows)]
pub(crate) mod reopen;
#[cfg(windows)]
pub(crate) mod share_audio;
