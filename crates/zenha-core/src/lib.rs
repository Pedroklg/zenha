//! Núcleo do cliente: sessão, áudio, captura e vídeo.
//!
//! A interface envia [`Command`] e lê [`Event`] por meio de [`SessionHandle`].
//! Nenhum tipo do SDK do LiveKit aparece na API pública desta crate.

mod api;
#[cfg(windows)]
mod audio_render;
mod backend;
mod backend_options;
#[cfg(any(windows, test))]
mod backend_support;
mod backoff;
mod capture;
mod chat;
mod clock;
mod cue;
#[cfg(windows)]
mod deep_filter;
#[cfg(windows)]
mod default_device;
#[cfg(any(windows, test))]
mod file_receive;
mod files;
#[cfg(windows)]
mod gpu_share;
mod handle;
#[cfg(windows)]
mod livekit_backend;
#[cfg(windows)]
mod livekit_files;
#[cfg(windows)]
mod livekit_video;
#[cfg(windows)]
mod mic_capture;
mod no_media;
mod noise;
mod participant_volume;
mod session;
#[cfg(windows)]
mod share_audio;
mod token;
mod video;

#[cfg(any(test, feature = "test-util"))]
pub mod testing;

pub use api::{
    AdminAction, AudioDevice, AudioDevices, Command, Event, FileTransfer, NoiseSuppression,
    Participant, RemovalReason, RoomKey, SessionConfig, SessionError, TransferState,
};
pub use backend::{BackendEvent, ConnectedRoom, MediaBackend};
pub use backend_options::{
    LiveKitBackendOptions, MicrophoneSource, TEST_PATTERN_SIZE, TEST_PATTERN_TARGET_ID,
    TEST_PATTERN_TARGET_NAME, TONE_FRAME_SAMPLES, TONE_SAMPLE_RATE, ToneGenerator,
    test_pattern_target,
};
pub use backoff::{ReconnectPolicy, backoff_delay};
pub use capture::{
    BgraFrame, CaptureEnd, FrameSink, FrameSource, Pacer, SlotSink, TestPatternSource,
    bgra_to_rgba, bgra_to_rgba_scaled, bgra_to_video_frame,
};
pub use chat::{CHAT_DELETE_TOPIC, CHAT_TOPIC};
pub use cue::{Cue, cue_samples};
pub use files::{FILE_TOPIC, is_image_mime, mime_for_name, received_path, sanitize_file_name};
pub use handle::SessionHandle;
#[cfg(windows)]
pub use livekit_backend::LiveKitBackend;
pub use no_media::NoMediaBackend;
pub use noise::{DENOISE_FRAME, Denoiser, GateReading, NoiseGate, RnnoiseDenoiser};
pub use token::{HttpTokenClient, TokenClient};
pub use video::{
    CAPTURE_THUMBNAIL_MAX_SIDE, CAPTURE_THUMBNAIL_TIMEOUT, CaptureKind, CaptureTarget, FrameSlot,
    PreviewGate, SHARE_PREVIEW_INTERVAL, SHARE_PREVIEW_MAX_SIDE, SharePreset, VideoFrame, Waker,
    fit_within,
};
