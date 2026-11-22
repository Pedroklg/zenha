//! Dedicated playback of a watched stream's audio.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use futures_util::StreamExt;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::audio_track::RtcAudioTrack;

use crate::audio::audio_render::{OutputStream, RENDER_CHANNELS, RENDER_SAMPLE_RATE};

use super::TaskGuard;

const STREAM_PRIME_SAMPLES: usize = RENDER_SAMPLE_RATE as usize * RENDER_CHANNELS * 40 / 1000;
const STREAM_MAX_SAMPLES: usize = RENDER_SAMPLE_RATE as usize * RENDER_CHANNELS * 200 / 1000;

pub(super) struct StreamPlayer {
    pub(super) track: RtcAudioTrack,
    _reader: TaskGuard,
    _output: OutputStream,
}

impl std::fmt::Debug for StreamPlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamPlayer")
    }
}

impl StreamPlayer {
    /// Needs a Tokio runtime. `gain` gives the factor applied to each `i16` sample.
    pub(super) fn start(
        track: RtcAudioTrack,
        device: Option<String>,
        gain: impl Fn() -> f32 + Send + 'static,
    ) -> Self {
        let queue: Arc<StdMutex<VecDeque<i16>>> = Arc::new(StdMutex::new(VecDeque::new()));
        let mut frames = NativeAudioStream::new(
            track.clone(),
            RENDER_SAMPLE_RATE as i32,
            RENDER_CHANNELS as i32,
        );
        let writer = queue.clone();
        let reader = tokio::spawn(async move {
            while let Some(frame) = frames.next().await {
                let mut queue = writer.lock().unwrap_or_else(PoisonError::into_inner);
                queue.extend(frame.data.iter().copied());
                if queue.len() > STREAM_MAX_SAMPLES {
                    // Late audio is dropped in whole frames so the channels stay aligned.
                    let excess =
                        (queue.len() - STREAM_PRIME_SAMPLES) / RENDER_CHANNELS * RENDER_CHANNELS;
                    queue.drain(..excess);
                }
            }
        });
        let mut primed = false;
        let output = OutputStream::start(device, move |out: &mut [f32]| {
            let gain = gain();
            let mut queue = queue.lock().unwrap_or_else(PoisonError::into_inner);
            // A short reserve is built before playing, and again after running dry.
            if !primed && queue.len() >= STREAM_PRIME_SAMPLES {
                primed = true;
            }
            if !primed {
                out.fill(0.0);
                return;
            }
            for slot in out.iter_mut() {
                *slot = queue
                    .pop_front()
                    .map_or(0.0, |sample| f32::from(sample) * gain);
            }
            if queue.is_empty() {
                primed = false;
            }
        });
        Self {
            track,
            _reader: TaskGuard(reader),
            _output: output,
        }
    }
}
