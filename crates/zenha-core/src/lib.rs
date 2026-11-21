//! Client core: session, audio, capture and video.
//!
//! The UI sends [`Command`] and reads [`Event`] through [`SessionHandle`].
//! No LiveKit SDK type appears in this crate's public API.

mod api;
mod audio;
mod backend;
mod chat;
mod clock;
mod files;
mod handle;
#[cfg(windows)]
mod livekit_backend;
mod net;
mod session;
mod video;

#[cfg(any(test, feature = "test-util"))]
pub mod testing;

pub use api::{
    AdminAction, AudioDevice, AudioDevices, Command, Event, FileTransfer, NoiseSuppression,
    Participant, RemovalReason, RoomKey, SessionConfig, SessionError, TransferState,
};
pub use audio::cue::{Cue, cue_samples};
pub use audio::noise::{DENOISE_FRAME, Denoiser, GateReading, NoiseGate, RnnoiseDenoiser};
pub use backend::no_media::NoMediaBackend;
pub use backend::options::{
    LiveKitBackendOptions, MicrophoneSource, TEST_PATTERN_SIZE, TEST_PATTERN_TARGET_ID,
    TEST_PATTERN_TARGET_NAME, TONE_FRAME_SAMPLES, TONE_SAMPLE_RATE, ToneGenerator,
    test_pattern_target,
};
pub use backend::{BackendEvent, ConnectedRoom, MediaBackend};
pub use chat::{CHAT_DELETE_TOPIC, CHAT_TOPIC};
pub use files::{FILE_TOPIC, is_image_mime, mime_for_name, received_path, sanitize_file_name};
pub use handle::SessionHandle;
#[cfg(windows)]
pub use livekit_backend::LiveKitBackend;
pub use net::backoff::{ReconnectPolicy, backoff_delay};
pub use net::install_crypto_provider;
pub use net::token::{HttpTokenClient, TokenClient};
pub use video::capture::{
    BgraFrame, CaptureEnd, FrameSink, FrameSource, Pacer, SlotSink, TestPatternSource,
    bgra_to_rgba, bgra_to_rgba_scaled, bgra_to_video_frame,
};
pub use video::{
    CAPTURE_THUMBNAIL_MAX_SIDE, CAPTURE_THUMBNAIL_TIMEOUT, CaptureKind, CaptureTarget, FrameSlot,
    PreviewGate, SHARE_PREVIEW_INTERVAL, SHARE_PREVIEW_MAX_SIDE, SharePreset, VideoFrame, Waker,
    fit_within,
};
