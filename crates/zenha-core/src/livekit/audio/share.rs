//! Áudio publicado junto com a transmissão de tela.

use std::sync::{Arc, Mutex as StdMutex};

use livekit::options::{self, TrackPublishOptions};
use livekit::prelude::{LocalAudioTrack, LocalTrack, Room, TrackSid, TrackSource};
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use tokio::sync::mpsc;

use crate::audio::share_audio::{
    AudioScope, SHARE_AUDIO_CHANNELS, SHARE_AUDIO_SAMPLE_RATE, ShareAudioCapture,
};
use crate::livekit::TaskGuard;

const SHARE_AUDIO_TRACK_NAME: &str = "screen-audio";
const SHARE_AUDIO_QUEUE_MS: u32 = 100;
const SHARE_AUDIO_BACKLOG_FRAMES: usize = 20;

/// Sound published along with the local screen share.
pub(in crate::livekit) struct ShareAudio {
    room: Arc<Room>,
    pub(in crate::livekit) sid: Arc<StdMutex<TrackSid>>,
    _pump: TaskGuard,
    _capture: ShareAudioCapture,
}

/// Publishes the sound of `scope` as the audio of the screen share.
pub(in crate::livekit) async fn publish_share_audio(
    room: Arc<Room>,
    scope: AudioScope,
) -> Option<ShareAudio> {
    let source = NativeAudioSource::new(
        AudioSourceOptions::default(),
        SHARE_AUDIO_SAMPLE_RATE,
        SHARE_AUDIO_CHANNELS,
        SHARE_AUDIO_QUEUE_MS,
    );
    let track = LocalAudioTrack::create_audio_track(
        SHARE_AUDIO_TRACK_NAME,
        RtcAudioSource::Native(source.clone()),
    );
    let publish_options = TrackPublishOptions {
        source: TrackSource::ScreenshareAudio,
        audio_encoding: Some(options::audio::MUSIC_STEREO.encoding),
        // Games and music are not speech: no silence detection, no redundancy.
        dtx: false,
        red: false,
        ..Default::default()
    };
    let publication = match room
        .local_participant()
        .publish_track(LocalTrack::Audio(track), publish_options)
        .await
    {
        Ok(publication) => publication,
        Err(err) => {
            tracing::warn!(%err, "could not publish the share audio");
            return None;
        }
    };
    let (tx, mut rx) = mpsc::channel::<Vec<i16>>(SHARE_AUDIO_BACKLOG_FRAMES);
    let capture = ShareAudioCapture::start(scope, move |frame: &[i16]| {
        let _ = tx.try_send(frame.to_vec());
    });
    let pump = tokio::spawn(async move {
        while let Some(samples) = rx.recv().await {
            let frame = AudioFrame {
                samples_per_channel: samples.len() as u32 / SHARE_AUDIO_CHANNELS,
                data: samples.into(),
                sample_rate: SHARE_AUDIO_SAMPLE_RATE,
                num_channels: SHARE_AUDIO_CHANNELS,
            };
            if let Err(err) = source.capture_frame(&frame).await {
                tracing::warn!(%err, "share audio stopped");
                return;
            }
        }
    });
    tracing::info!(?scope, "share audio published");
    Some(ShareAudio {
        room,
        sid: Arc::new(StdMutex::new(publication.sid())),
        _pump: TaskGuard(pump),
        _capture: capture,
    })
}

pub(in crate::livekit) async fn stop_share_audio(audio: ShareAudio) {
    let ShareAudio { room, sid, .. } = audio;
    let sid = sid
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Err(err) = room.local_participant().unpublish_track(&sid).await {
        tracing::warn!(%err, "unpublishing the share audio failed");
    }
}
