//! Publicação do microfone (plataforma, aprimorado ou tom de teste) e detecção de fala local.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use livekit::options::{self, TrackPublishOptions};
use livekit::prelude::{LocalAudioTrack, LocalTrack, PlatformAudio, Room, TrackSource};
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::native::apm::AudioProcessingModule;
use livekit::webrtc::stats::RtcStats;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::MissedTickBehavior;

use super::{EnhancedMic, Microphone, Selection};
use crate::api::NoiseSuppression;
use crate::audio::deep_filter::DeepFilterDenoiser;
use crate::audio::mic_capture::{CAPTURE_SAMPLE_RATE, MicCapture};
use crate::audio::noise::{Denoiser, NoiseGate, RnnoiseDenoiser};
use crate::backend::BackendEvent;
use crate::backend::options::{TONE_FRAME_SAMPLES, TONE_SAMPLE_RATE, ToneGenerator};
use crate::backend::support::{
    LevelMeter, LocalSpeech, SPEAKING_AUDIO_LEVEL, enhanced_voice, gate_threshold,
};
use crate::livekit::{Flags, TaskGuard};

const MICROPHONE_TRACK_NAME: &str = "microphone";
const TONE_QUEUE_MS: u32 = 100;
const ENHANCED_QUEUE_MS: u32 = 60;
const ENHANCED_BACKLOG_FRAMES: usize = 20;
const SPEECH_POLL: Duration = Duration::from_millis(100);

pub(in crate::livekit) async fn publish_selected_microphone(
    room: &Room,
    audio: &PlatformAudio,
    selection: &Selection,
    events: &UnboundedSender<BackendEvent>,
    flags: &Arc<Flags>,
) -> Microphone {
    tracing::info!(mode = ?selection.noise, "publishing the microphone");
    if selection.noise.uses_own_capture() {
        if let Some(mic) = publish_enhanced(
            room,
            selection.noise,
            selection.input.clone(),
            LocalSpeaker::new(room, events, flags),
        )
        .await
        {
            return Microphone::Enhanced { _mic: mic };
        }
        tracing::warn!("enhanced microphone unavailable, falling back to the platform one");
    }
    match publish_microphone(room, audio).await {
        Some(track) => Microphone::Platform {
            _speech: watch_speech(track, LocalSpeaker::new(room, events, flags)),
        },
        None => Microphone::None,
    }
}

/// Reports the local participant's speaking state from local audio, ahead of the server.
pub(in crate::livekit) struct LocalSpeaker {
    speech: LocalSpeech,
    identity: String,
    events: UnboundedSender<BackendEvent>,
    flags: Arc<Flags>,
}

impl LocalSpeaker {
    pub(in crate::livekit) fn new(
        room: &Room,
        events: &UnboundedSender<BackendEvent>,
        flags: &Arc<Flags>,
    ) -> Self {
        Self {
            speech: LocalSpeech::default(),
            identity: String::from(room.local_participant().identity()),
            events: events.clone(),
            flags: flags.clone(),
        }
    }

    fn silenced(&self) -> bool {
        self.flags.microphone_muted.load(Ordering::Relaxed)
            || self.flags.output_muted.load(Ordering::Relaxed)
    }

    fn report(&mut self, voice: bool) {
        let silenced = self.silenced();
        let change = self.speech.update(voice, silenced, Instant::now());
        self.send(change);
    }

    fn send(&self, change: Option<bool>) {
        if let Some(speaking) = change {
            let _ = self.events.send(BackendEvent::SpeakingChanged {
                identity: self.identity.clone(),
                speaking,
            });
        }
    }
}

impl Drop for LocalSpeaker {
    fn drop(&mut self) {
        let change = self.speech.update(false, true, Instant::now());
        self.send(change);
    }
}

/// The platform microphone hands no samples to the app, so its level comes from the sender stats.
pub(in crate::livekit) fn watch_speech(
    track: LocalAudioTrack,
    mut speaker: LocalSpeaker,
) -> TaskGuard {
    TaskGuard(tokio::spawn(async move {
        let mut ticks = tokio::time::interval(SPEECH_POLL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            let voice = !speaker.silenced()
                && track.get_stats().await.is_ok_and(|stats| {
                    stats.iter().any(|stat| {
                        matches!(stat, RtcStats::MediaSource(source)
                            if source.audio.audio_level >= SPEAKING_AUDIO_LEVEL)
                    })
                });
            speaker.report(voice);
        }
    }))
}

/// `ZENHA_ENHANCED_TEST_TONE=hz,amplitude` (diagnostic) replaces the cleaned audio with a tone.
fn enhanced_test_tone() -> Option<ToneGenerator> {
    let value = std::env::var("ZENHA_ENHANCED_TEST_TONE").ok()?;
    let (hz, amplitude) = value.split_once(',')?;
    Some(ToneGenerator::new(
        hz.trim().parse().ok()?,
        amplitude.trim().parse().ok()?,
        CAPTURE_SAMPLE_RATE,
    ))
}

fn build_denoiser(mode: NoiseSuppression) -> Box<dyn Denoiser> {
    if mode == NoiseSuppression::Maximum {
        match DeepFilterDenoiser::new() {
            Ok(denoiser) => {
                tracing::info!(engine = "deepfilternet", "noise suppression engine ready");
                return Box::new(denoiser);
            }
            Err(err) => {
                tracing::warn!(%err, "deepfilternet unavailable, falling back to rnnoise");
            }
        }
    }
    tracing::info!(engine = "rnnoise", "noise suppression engine ready");
    Box::new(RnnoiseDenoiser::new())
}

/// Own capture, gain control and neural noise suppression, pushed as a manual audio source.
async fn publish_enhanced(
    room: &Room,
    mode: NoiseSuppression,
    input: Option<String>,
    mut speaker: LocalSpeaker,
) -> Option<EnhancedMic> {
    let events = speaker.events.clone();
    let noise_gate = speaker.flags.noise_gate.clone();
    let source = NativeAudioSource::new(
        AudioSourceOptions::default(),
        CAPTURE_SAMPLE_RATE,
        1,
        ENHANCED_QUEUE_MS,
    );
    let track = LocalAudioTrack::create_audio_track(
        MICROPHONE_TRACK_NAME,
        RtcAudioSource::Native(source.clone()),
    );
    let publish_options = TrackPublishOptions {
        source: TrackSource::Microphone,
        audio_encoding: Some(options::audio::SPEECH.encoding),
        ..Default::default()
    };
    if let Err(err) = room
        .local_participant()
        .publish_track(LocalTrack::Audio(track), publish_options)
        .await
    {
        tracing::warn!(%err, "could not publish the enhanced microphone");
        return None;
    }

    let (tx, mut rx) = mpsc::channel::<Vec<i16>>(ENHANCED_BACKLOG_FRAMES);
    let mut tone = enhanced_test_tone();
    if tone.is_some() {
        tracing::info!("enhanced microphone replaced by the diagnostic tone");
    }
    let capture = MicCapture::start(input, move || {
        let mut apm = AudioProcessingModule::new(false, true, true, false);
        let mut denoiser = build_denoiser(mode);
        let mut gate = NoiseGate::new();
        let mut meter = LevelMeter::default();
        move |frame: &mut [i16]| {
            if let Err(err) = apm.process_stream(frame, CAPTURE_SAMPLE_RATE as i32, 1) {
                tracing::debug!(%err, "gain control failed for a frame");
            }
            let speech = denoiser.process(frame);
            let threshold = gate_threshold(noise_gate.load(Ordering::Relaxed));
            let reading = gate.process(frame, threshold, speech);
            speaker.report(enhanced_voice(reading, threshold));
            if let Some((level_db, gate_open)) = meter.push(reading) {
                let _ = events.send(BackendEvent::MicLevel {
                    level_db,
                    gate_open,
                });
            }
            if let Some(tone) = &mut tone {
                tone.fill(frame);
            }
            // A full queue means the sender stalled; dropping keeps the delay bounded.
            let _ = tx.try_send(frame.to_vec());
        }
    });
    let pump = tokio::spawn(async move {
        while let Some(samples) = rx.recv().await {
            let frame = AudioFrame {
                samples_per_channel: samples.len() as u32,
                data: samples.into(),
                sample_rate: CAPTURE_SAMPLE_RATE,
                num_channels: 1,
            };
            if let Err(err) = source.capture_frame(&frame).await {
                tracing::warn!(%err, "enhanced microphone stopped");
                return;
            }
        }
    });
    Some(EnhancedMic {
        _pump: TaskGuard(pump),
        _capture: capture,
    })
}

/// Publishes the sound of `scope` as the audio of the screen share.

async fn publish_microphone(room: &Room, audio: &PlatformAudio) -> Option<LocalAudioTrack> {
    if audio.recording_devices().next().is_none() {
        tracing::warn!("no microphone found, staying in listen-only mode");
        return None;
    }
    let track = LocalAudioTrack::create_audio_track(MICROPHONE_TRACK_NAME, audio.rtc_source());
    let publish_options = TrackPublishOptions {
        source: TrackSource::Microphone,
        audio_encoding: Some(options::audio::SPEECH.encoding),
        ..Default::default()
    };
    match room
        .local_participant()
        .publish_track(LocalTrack::Audio(track.clone()), publish_options)
        .await
    {
        Ok(_) => Some(track),
        Err(err) => {
            tracing::warn!(%err, "could not publish the microphone, staying in listen-only mode");
            None
        }
    }
}

pub(in crate::livekit) async fn publish_tone(
    room: &Room,
    hz: f64,
    amplitude: f64,
    speaker: LocalSpeaker,
) -> Option<Microphone> {
    let source = NativeAudioSource::new(
        AudioSourceOptions::default(),
        TONE_SAMPLE_RATE,
        1,
        TONE_QUEUE_MS,
    );
    let track = LocalAudioTrack::create_audio_track(
        MICROPHONE_TRACK_NAME,
        RtcAudioSource::Native(source.clone()),
    );
    let publish_options = TrackPublishOptions {
        source: TrackSource::Microphone,
        audio_encoding: Some(options::audio::SPEECH.encoding),
        // Opus treats a steady tone as background noise and stops sending under DTX.
        dtx: false,
        red: false,
        ..Default::default()
    };
    if let Err(err) = room
        .local_participant()
        .publish_track(LocalTrack::Audio(track.clone()), publish_options)
        .await
    {
        tracing::warn!(%err, "could not publish the test tone");
        return None;
    }
    tracing::info!(
        hz,
        amplitude,
        "publishing a synthetic test tone as the microphone"
    );
    Some(Microphone::Tone {
        _task: TaskGuard(tokio::spawn(tone_loop(source, hz, amplitude))),
        _speech: watch_speech(track, speaker),
    })
}

/// `capture_frame` waits for room in the 100 ms queue, which paces the loop in real time.
async fn tone_loop(source: NativeAudioSource, hz: f64, amplitude: f64) {
    let mut generator = ToneGenerator::new(hz, amplitude, TONE_SAMPLE_RATE);
    let mut samples = vec![0i16; TONE_FRAME_SAMPLES];
    loop {
        generator.fill(&mut samples);
        let frame = AudioFrame {
            data: samples.as_slice().into(),
            sample_rate: TONE_SAMPLE_RATE,
            num_channels: 1,
            samples_per_channel: TONE_FRAME_SAMPLES as u32,
        };
        if let Err(err) = source.capture_frame(&frame).await {
            tracing::warn!(%err, "test tone stopped");
            return;
        }
    }
}
