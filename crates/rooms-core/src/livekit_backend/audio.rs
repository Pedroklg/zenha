//! Publicação de áudio: microfone de plataforma/aprimorado, tom de teste e áudio da tela.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use livekit::options::{self, TrackPublishOptions};
use livekit::prelude::{
    AudioError, AudioProcessingOptions, LocalAudioTrack, LocalTrack, PlatformAudio,
    PlayoutDeviceInfo, RecordingDeviceInfo, Room, TrackSid, TrackSource,
};
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::native::apm::AudioProcessingModule;
use livekit::webrtc::stats::RtcStats;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::MissedTickBehavior;

use crate::api::{AudioDevice, AudioDevices, NoiseSuppression, SessionError};
use crate::backend::BackendEvent;
use crate::backend_options::{TONE_FRAME_SAMPLES, TONE_SAMPLE_RATE, ToneGenerator};
use crate::backend_support::{
    LevelMeter, LocalSpeech, SPEAKING_AUDIO_LEVEL, audio_device, enhanced_voice, gate_threshold,
    unique_devices,
};
use crate::deep_filter::DeepFilterDenoiser;
use crate::mic_capture::{CAPTURE_SAMPLE_RATE, MicCapture};
use crate::noise::{Denoiser, NoiseGate, RnnoiseDenoiser};
use crate::participant_volume::volume_gain;
use crate::share_audio::{
    AudioScope, SHARE_AUDIO_CHANNELS, SHARE_AUDIO_SAMPLE_RATE, ShareAudioCapture,
};

use super::room::{apply_microphone_mute, apply_participant_volume, blocking};
use super::{Flags, LiveKitBackend, TaskGuard};

const MICROPHONE_TRACK_NAME: &str = "microphone";
const TONE_QUEUE_MS: u32 = 100;
const ENHANCED_QUEUE_MS: u32 = 60;
const SHARE_AUDIO_TRACK_NAME: &str = "screen-audio";
const SHARE_AUDIO_QUEUE_MS: u32 = 100;
const SHARE_AUDIO_BACKLOG_FRAMES: usize = 20;
const ENHANCED_BACKLOG_FRAMES: usize = 20;
const SPEECH_POLL: Duration = Duration::from_millis(100);

/// Sound published along with the local screen share.
pub(super) struct ShareAudio {
    room: Arc<Room>,
    pub(super) sid: Arc<StdMutex<TrackSid>>,
    _pump: TaskGuard,
    _capture: ShareAudioCapture,
}

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
    async fn republish_microphone(&self) {
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
        let result = blocking(move || apply_output(&audio, id.as_deref(), true)).await;
        match result.and_then(|applied| applied.map(|_| ())) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.update_selection(|s| s.output = previous);
                Err(err)
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

fn input_devices(infos: &[RecordingDeviceInfo]) -> Vec<AudioDevice> {
    unique_devices(
        infos
            .iter()
            .map(|info| audio_device(info.id.as_str(), &info.name))
            .collect(),
    )
}

fn output_devices(infos: &[PlayoutDeviceInfo]) -> Vec<AudioDevice> {
    unique_devices(
        infos
            .iter()
            .map(|info| audio_device(info.id.as_str(), &info.name))
            .collect(),
    )
}

/// `running` selects the hot-swap call, which stops and restarts active audio;
/// before the room starts the plain setter is the right one.
fn apply_input(
    audio: &PlatformAudio,
    requested: Option<&str>,
    running: bool,
) -> Result<Option<String>, SessionError> {
    let fallback;
    let requested = match requested {
        Some(requested) => requested,
        None => match crate::default_device::default_input_id() {
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

fn apply_output(
    audio: &PlatformAudio,
    requested: Option<&str>,
    running: bool,
) -> Result<Option<String>, SessionError> {
    let fallback;
    let requested = match requested {
        Some(requested) => requested,
        None => match crate::default_device::default_output_id() {
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

pub(super) fn apply_selection(audio: &PlatformAudio, selection: &Selection) {
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

fn processing_for(mode: NoiseSuppression) -> AudioProcessingOptions {
    AudioProcessingOptions {
        noise_suppression: mode == NoiseSuppression::Standard,
        ..AudioProcessingOptions::default()
    }
}

pub(super) fn open_audio(mode: NoiseSuppression) -> Option<PlatformAudio> {
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

pub(super) async fn publish_selected_microphone(
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
pub(super) struct LocalSpeaker {
    speech: LocalSpeech,
    identity: String,
    events: UnboundedSender<BackendEvent>,
    flags: Arc<Flags>,
}

impl LocalSpeaker {
    pub(super) fn new(
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
pub(super) fn watch_speech(track: LocalAudioTrack, mut speaker: LocalSpeaker) -> TaskGuard {
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

/// `ROOMS_ENHANCED_TEST_TONE=hz,amplitude` (diagnostic) replaces the cleaned audio with a tone.
fn enhanced_test_tone() -> Option<ToneGenerator> {
    let value = std::env::var("ROOMS_ENHANCED_TEST_TONE").ok()?;
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
pub(super) async fn publish_share_audio(room: Arc<Room>, scope: AudioScope) -> Option<ShareAudio> {
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

pub(super) async fn stop_share_audio(audio: ShareAudio) {
    let ShareAudio { room, sid, .. } = audio;
    let sid = sid
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Err(err) = room.local_participant().unpublish_track(&sid).await {
        tracing::warn!(%err, "unpublishing the share audio failed");
    }
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

pub(super) async fn publish_tone(
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

/// Diagnostic: logs once per second the level and dominant frequency of the decoded remote audio.
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
