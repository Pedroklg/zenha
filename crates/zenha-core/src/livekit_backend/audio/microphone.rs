//! Microphone publishing (platform, enhanced or test tone) and local speech detection.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
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
use crate::audio::mic_capture::{CAPTURE_SAMPLE_RATE, MicCapture};
use crate::audio::noise::{Denoiser, NoiseGate, RnnoiseDenoiser};
use crate::audio::share_audio::{
    AudioScope, SHARE_AUDIO_CHANNELS, SHARE_AUDIO_SAMPLE_RATE, ShareAudioCapture,
};
use crate::backend::BackendEvent;
use crate::backend::options::{TONE_FRAME_SAMPLES, TONE_SAMPLE_RATE, ToneGenerator};
use crate::backend::support::{
    LevelMeter, LocalSpeech, SPEAKING_AUDIO_LEVEL, enhanced_voice, gate_threshold,
};
use crate::livekit_backend::{Flags, TaskGuard};

const MICROPHONE_TRACK_NAME: &str = "microphone";
const TONE_QUEUE_MS: u32 = 100;
const ENHANCED_QUEUE_MS: u32 = 60;
const ENHANCED_BACKLOG_FRAMES: usize = 20;
const SPEECH_POLL: Duration = Duration::from_millis(100);

pub(in crate::livekit_backend) async fn publish_selected_microphone(
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
            selection.input.clone(),
            selection.noise == NoiseSuppression::Strong,
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
pub(in crate::livekit_backend) struct LocalSpeaker {
    speech: LocalSpeech,
    identity: String,
    events: UnboundedSender<BackendEvent>,
    flags: Arc<Flags>,
}

impl LocalSpeaker {
    pub(in crate::livekit_backend) fn new(
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
pub(in crate::livekit_backend) fn watch_speech(
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

fn build_denoiser() -> Box<dyn Denoiser> {
    tracing::info!(
        engine = "rnnoise-v0.2-little",
        "noise suppression engine ready"
    );
    Box::new(RnnoiseDenoiser::new())
}

/// `ZENHA_ECHO_TEST=gain` (diagnostic) turns echo cancellation on and replaces the microphone with
/// this app's own sound times `gain`, 60 ms late, as headphones leaking into the microphone would.
fn echo_test_gain() -> Option<f32> {
    std::env::var("ZENHA_ECHO_TEST").ok()?.trim().parse().ok()
}

struct EchoTest {
    leaked: Arc<StdMutex<std::collections::VecDeque<i16>>>,
    _capture: ShareAudioCapture,
    before: f64,
    after: f64,
    frames: u32,
}

impl EchoTest {
    const DELAY_SAMPLES: usize = 60 * CAPTURE_SAMPLE_RATE as usize / 1000;
    const REPORT_FRAMES: u32 = 200;

    fn start(gain: f32) -> Self {
        let leaked = Arc::new(StdMutex::new(std::collections::VecDeque::from(vec![
            0i16;
            Self::DELAY_SAMPLES
        ])));
        let sink = leaked.clone();
        let capture =
            ShareAudioCapture::start(AudioScope::Process(std::process::id()), move |frame| {
                let mut queue = sink.lock().unwrap_or_else(PoisonError::into_inner);
                for pair in frame.chunks(SHARE_AUDIO_CHANNELS as usize) {
                    let mono = pair.iter().map(|&s| f32::from(s)).sum::<f32>() / pair.len() as f32;
                    queue.push_back((mono * gain) as i16);
                }
                let excess = queue.len().saturating_sub(Self::DELAY_SAMPLES * 4);
                queue.drain(..excess);
            });
        tracing::info!(
            gain,
            "echo test: microphone replaced by the app's own sound"
        );
        Self {
            leaked,
            _capture: capture,
            before: 0.0,
            after: 0.0,
            frames: 0,
        }
    }

    fn replace_microphone(&mut self, frame: &mut [i16]) {
        let mut queue = self.leaked.lock().unwrap_or_else(PoisonError::into_inner);
        for sample in frame.iter_mut() {
            *sample = queue.pop_front().unwrap_or(0);
        }
        drop(queue);
        self.before += energy(frame);
    }

    fn measure(&mut self, frame: &[i16]) {
        self.after += energy(frame);
        self.frames += 1;
        if self.frames == Self::REPORT_FRAMES {
            let samples = f64::from(self.frames) * frame.len() as f64;
            let before = (self.before / samples).sqrt();
            let after = (self.after / samples).sqrt();
            tracing::info!(
                before_rms = format!("{before:.1}"),
                after_rms = format!("{after:.1}"),
                reduction_db = format!("{:.1}", 20.0 * (before / after.max(1.0)).log10()),
                queued = self
                    .leaked
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .len(),
                "echo test"
            );
            (self.before, self.after, self.frames) = (0.0, 0.0, 0);
        }
    }
}

fn energy(frame: &[i16]) -> f64 {
    frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum()
}

/// Removes from the own capture what this app plays (voices, stream sound, cues),
/// taken from the loopback of its own process as the echo reference.
struct EchoCanceller {
    apm: Arc<StdMutex<AudioProcessingModule>>,
    _reference: ShareAudioCapture,
}

impl EchoCanceller {
    fn start() -> Self {
        let apm = Arc::new(StdMutex::new(AudioProcessingModule::new(
            true, false, false, false,
        )));
        let render = apm.clone();
        let reference =
            ShareAudioCapture::start(AudioScope::Process(std::process::id()), move |frame| {
                let mut frame = frame.to_vec();
                let result = render
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .process_reverse_stream(
                        &mut frame,
                        SHARE_AUDIO_SAMPLE_RATE as i32,
                        SHARE_AUDIO_CHANNELS as i32,
                    );
                if let Err(err) = result {
                    tracing::debug!(%err, "echo reference frame rejected");
                }
            });
        tracing::info!("echo cancellation on for the enhanced microphone");
        Self {
            apm,
            _reference: reference,
        }
    }

    fn process(&self, frame: &mut [i16]) {
        let result = self
            .apm
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process_stream(frame, CAPTURE_SAMPLE_RATE as i32, 1);
        if let Err(err) = result {
            tracing::debug!(%err, "echo cancellation failed for a frame");
        }
    }
}

/// Own capture, gain control and neural noise suppression, pushed as a manual audio source.
/// `strong` adds the WebRTC suppression after the neural one.
async fn publish_enhanced(
    room: &Room,
    input: Option<String>,
    strong: bool,
    mut speaker: LocalSpeaker,
) -> Option<EnhancedMic> {
    let events = speaker.events.clone();
    let noise_gate = speaker.flags.noise_gate.clone();
    let echo_test = echo_test_gain();
    let echo = echo_test.is_some() || speaker.flags.echo_cancellation.load(Ordering::SeqCst);
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
        let mut apm = AudioProcessingModule::new(false, true, true, strong);
        let canceller = echo.then(EchoCanceller::start);
        let mut leak = echo_test.map(EchoTest::start);
        let mut denoiser = build_denoiser();
        let mut gate = NoiseGate::new();
        let mut meter = LevelMeter::default();
        move |frame: &mut [i16]| {
            if speaker.silenced() {
                speaker.report(false);
                return;
            }
            if let Some(leak) = &mut leak {
                leak.replace_microphone(frame);
            }
            if let Some(canceller) = &canceller {
                canceller.process(frame);
            }
            if let Some(leak) = &mut leak {
                leak.measure(frame);
            }
            let speech = denoiser.process(frame);
            if let Err(err) = apm.process_stream(frame, CAPTURE_SAMPLE_RATE as i32, 1) {
                tracing::debug!(%err, "gain control failed for a frame");
            }
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

pub(in crate::livekit_backend) async fn publish_tone(
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

#[cfg(test)]
mod tests {
    use livekit::webrtc::native::apm::AudioProcessingModule;

    const FRAME: usize = 480;

    fn rms(samples: &[i16]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        (sum / samples.len().max(1) as f64).sqrt()
    }

    /// Simulated room: the microphone hears the played sound 40 ms later at a third of its level.
    #[test]
    fn echo_canceller_removes_a_delayed_copy_of_the_reference() {
        let mut apm = AudioProcessingModule::new(true, false, false, false);
        let mut seed = 12345u32;
        let mut played: Vec<Vec<i16>> = Vec::new();
        let (mut before, mut after) = (Vec::new(), Vec::new());
        for index in 0usize..800 {
            let mut reverse = vec![0i16; FRAME * 2];
            for pair in reverse.chunks_mut(2) {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let value = ((seed >> 16) as i16) / 4;
                pair.fill(value);
            }
            played.push(reverse.iter().step_by(2).copied().collect());
            apm.process_reverse_stream(&mut reverse, 48_000, 2).unwrap();
            let mut capture: Vec<i16> = match index.checked_sub(4) {
                Some(old) => played[old].iter().map(|&s| s / 3).collect(),
                None => vec![0; FRAME],
            };
            if index >= 600 {
                before.extend_from_slice(&capture);
            }
            apm.process_stream(&mut capture, 48_000, 1).unwrap();
            if index >= 600 {
                after.extend_from_slice(&capture);
            }
        }
        let reduction_db = 20.0 * (rms(&before) / rms(&after).max(1.0)).log10();
        println!("echo reduction {reduction_db:.1} dB");
        assert!(
            reduction_db > 20.0,
            "echo only reduced by {reduction_db:.1} dB"
        );
    }
}
