use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Once, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use livekit::options::{self, TrackPublishOptions};
use livekit::prelude::{
    AudioError, AudioProcessingOptions, DataPacket, DataPacketKind, LocalAudioTrack, LocalTrack,
    Participant as SdkParticipant, PlatformAudio, PlayoutDeviceInfo, RecordingDeviceInfo,
    RemoteParticipant, RemoteTrack, RemoteTrackPublication, Room, RoomError, RoomEvent,
    RoomOptions, TrackKind, TrackPublication, TrackSid, TrackSource,
};
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::audio_track::RtcAudioTrack;
use livekit::webrtc::native::apm::AudioProcessingModule;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::api::{
    AudioDevice, AudioDevices, FileTransfer, NoiseSuppression, Participant, SessionError,
};
use crate::audio_render::{OutputStream, RENDER_CHANNELS, RENDER_SAMPLE_RATE};
use crate::backend::{BackendEvent, ConnectedRoom, MediaBackend};
use crate::backend_options::{
    LiveKitBackendOptions, MicrophoneSource, TEST_PATTERN_SIZE, TEST_PATTERN_TARGET_ID,
    TONE_FRAME_SAMPLES, TONE_SAMPLE_RATE, ToneGenerator, test_pattern_target,
};
use crate::backend_support::{
    GATE_OFF, LevelMeter, SpeakingTracker, audio_device, display_name, gate_threshold,
    sanitize_error, unique_devices,
};
use crate::capture::{FrameSource, TestPatternSource};
use crate::deep_filter::DeepFilterDenoiser;
use crate::file_receive::FileReceiver;
use crate::files::FILE_TOPIC;
use crate::livekit_files;
use crate::livekit_video::{self, DesktopCaptureSource, LocalShare, Watch};
use crate::mic_capture::{CAPTURE_SAMPLE_RATE, MicCapture};
use crate::noise::{Denoiser, NoiseGate, RnnoiseDenoiser};
use crate::share_audio::{
    AudioScope, SHARE_AUDIO_CHANNELS, SHARE_AUDIO_SAMPLE_RATE, ShareAudioCapture,
};
use crate::video::{CaptureTarget, FrameSlot, SharePreset};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const MICROPHONE_TRACK_NAME: &str = "microphone";
const TONE_QUEUE_MS: u32 = 100;
const ENHANCED_QUEUE_MS: u32 = 60;
const SHARE_AUDIO_TRACK_NAME: &str = "screen-audio";
const SHARE_AUDIO_QUEUE_MS: u32 = 100;
const SHARE_AUDIO_BACKLOG_FRAMES: usize = 20;
const ENHANCED_BACKLOG_FRAMES: usize = 20;

#[derive(Debug, Default)]
struct Flags {
    microphone_muted: AtomicBool,
    output_muted: AtomicBool,
    /// 100 minus the stream volume, so the zero default means full volume.
    stream_attenuation: Arc<AtomicU8>,
    /// Noise gate threshold in dBFS; the zero default means the gate is off.
    noise_gate: Arc<AtomicI8>,
    /// Output device of the stream sound; `None` is the Windows default.
    stream_output: StdMutex<Option<String>>,
    /// Sound of the screen share being watched.
    stream_player: StdMutex<Option<StreamPlayer>>,
}

impl Flags {
    /// Plays `track` through the app's own output so its volume can be set apart from voices.
    fn play_stream(&self, track: RtcAudioTrack) {
        let device = self
            .stream_output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let player = StreamPlayer::start(track, device, self.stream_attenuation.clone());
        *self
            .stream_player
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(player);
    }

    fn stop_stream(&self) -> Option<RtcAudioTrack> {
        self.stream_player
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(|player| player.track)
    }

    fn set_stream_output(&self, device: Option<String>) {
        *self
            .stream_output
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = device;
        if let Some(track) = self.stop_stream() {
            self.play_stream(track);
        }
    }
}

const STREAM_PRIME_SAMPLES: usize = RENDER_SAMPLE_RATE as usize * RENDER_CHANNELS * 40 / 1000;
const STREAM_MAX_SAMPLES: usize = RENDER_SAMPLE_RATE as usize * RENDER_CHANNELS * 200 / 1000;

struct StreamPlayer {
    track: RtcAudioTrack,
    _reader: TaskGuard,
    _output: OutputStream,
}

impl std::fmt::Debug for StreamPlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamPlayer")
    }
}

impl StreamPlayer {
    /// Needs a Tokio runtime. The engine keeps decoding the track but no longer plays it.
    fn start(track: RtcAudioTrack, device: Option<String>, attenuation: Arc<AtomicU8>) -> Self {
        track.set_enabled(false);
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
            let volume =
                f32::from(100u8.saturating_sub(attenuation.load(Ordering::SeqCst))) / 100.0;
            // Squared so the slider follows how loudness is heard.
            let gain = volume * volume / 32768.0;
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

/// Sound published along with the local screen share.
struct ShareAudio {
    room: Arc<Room>,
    sid: TrackSid,
    _pump: TaskGuard,
    _capture: ShareAudioCapture,
}

#[derive(Debug, Clone, Default)]
struct Selection {
    input: Option<String>,
    output: Option<String>,
    noise: NoiseSuppression,
}

/// Keeps alive whatever feeds the published microphone track.
enum Microphone {
    None,
    Platform,
    Enhanced { _mic: EnhancedMic },
    Tone { _task: TaskGuard },
}

struct EnhancedMic {
    _pump: TaskGuard,
    _capture: MicCapture,
}

struct TaskGuard(JoinHandle<()>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Connection {
    room: Arc<Room>,
    events_tx: UnboundedSender<BackendEvent>,
    // Held so the platform audio device stays alive until the room is closed.
    audio: Option<PlatformAudio>,
    events: JoinHandle<()>,
    mic: Microphone,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.events.abort();
    }
}

struct RoomGuard(Option<Arc<Room>>);

impl RoomGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for RoomGuard {
    fn drop(&mut self) {
        let Some(room) = self.0.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if timeout(CLOSE_TIMEOUT, room.close()).await.is_err() {
                        tracing::warn!("closing an abandoned room timed out");
                    }
                });
            }
            Err(_) => tracing::warn!("no runtime to close an abandoned room"),
        }
    }
}

/// Backend de mídia real: sala do LiveKit, microfone e reprodução pelo áudio da plataforma.
pub struct LiveKitBackend {
    connection: Mutex<Option<Connection>>,
    flags: Arc<Flags>,
    selection: StdMutex<Selection>,
    local_share: Mutex<Option<LocalShare>>,
    local_share_audio: Mutex<Option<ShareAudio>>,
    watch: Arc<Watch>,
    targets: StdMutex<Vec<CaptureTarget>>,
    files: Arc<FileReceiver>,
    options: LiveKitBackendOptions,
}

impl LiveKitBackend {
    pub fn new() -> Self {
        Self::with_options(LiveKitBackendOptions::default())
    }

    /// Backend with the test hooks of `options`; `new` is the normal client behaviour.
    pub fn with_options(options: LiveKitBackendOptions) -> Self {
        Self {
            connection: Mutex::new(None),
            flags: Arc::new(Flags::default()),
            selection: StdMutex::new(Selection::default()),
            local_share: Mutex::new(None),
            local_share_audio: Mutex::new(None),
            watch: Arc::new(Watch::default()),
            targets: StdMutex::new(Vec::new()),
            files: Arc::new(FileReceiver::for_process()),
            options,
        }
    }

    async fn current_room_and_events(&self) -> Option<(Arc<Room>, UnboundedSender<BackendEvent>)> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|connection| (connection.room.clone(), connection.events_tx.clone()))
    }

    async fn stop_local_share(&self) {
        let audio = self.local_share_audio.lock().await.take();
        if let Some(audio) = audio {
            stop_share_audio(audio).await;
        }
        let share = self.local_share.lock().await.take();
        if let Some(share) = share {
            livekit_video::stop_share(share).await;
        }
    }

    async fn resolve_target(&self, target_id: &str) -> Result<CaptureTarget, SessionError> {
        let find = |targets: &[CaptureTarget]| targets.iter().find(|t| t.id == target_id).cloned();
        let cached = find(&self.targets.lock().unwrap_or_else(PoisonError::into_inner));
        if let Some(target) = cached {
            return Ok(target);
        }
        let fresh = self.capture_targets().await?;
        find(&fresh).ok_or_else(|| SessionError::TargetNotFound(target_id.to_owned()))
    }

    fn selection(&self) -> Selection {
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

    async fn current_audio(&self) -> Option<PlatformAudio> {
        self.connection
            .lock()
            .await
            .as_ref()
            .and_then(|connection| connection.audio.clone())
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
            &self.flags.noise_gate,
        )
        .await;
        if self.flags.microphone_muted.load(Ordering::SeqCst) {
            apply_microphone_mute(&connection.room, true);
        }
    }

    async fn current_room(&self) -> Option<Arc<Room>> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|connection| connection.room.clone())
    }

    async fn shutdown(&self) {
        self.stop_local_share().await;
        self.watch.clear();
        self.flags.stop_stream();
        let connection = self.connection.lock().await.take();
        let Some(connection) = connection else {
            return;
        };
        connection.events.abort();
        match timeout(CLOSE_TIMEOUT, connection.room.close()).await {
            Ok(Ok(())) | Ok(Err(RoomError::AlreadyClosed)) => {}
            Ok(Err(err)) => tracing::warn!(%err, "closing the room failed"),
            Err(_) => tracing::warn!("closing the room timed out"),
        }
    }

    async fn establish(&self, url: &str, token: &str) -> Result<ConnectedRoom, SessionError> {
        self.shutdown().await;
        install_crypto_provider();

        let selection = self.selection();
        let audio = if self.options.is_tone() {
            None
        } else {
            open_audio(selection.noise)
        };
        let mut room_options = RoomOptions::default();
        room_options.auto_subscribe = false;
        let (room, room_events) = Room::connect(url, token, room_options)
            .await
            .map_err(|err| {
                SessionError::Connection(sanitize_error(
                    &format!("could not join the room: {err}"),
                    token,
                ))
            })?;
        let room = Arc::new(room);
        let mut guard = RoomGuard(Some(room.clone()));
        // The voice engine starts while connecting and resets both devices to the Windows
        // default *communications* endpoints, so the selection only sticks from here on.
        if let Some(audio) = &audio {
            apply_selection(audio, &selection);
        }
        self.flags.set_stream_output(selection.output.clone());

        let (tx, rx) = mpsc::unbounded_channel();
        let mic = match self.options.microphone {
            MicrophoneSource::Device => match &audio {
                Some(audio) => {
                    publish_selected_microphone(
                        &room,
                        audio,
                        &selection,
                        &tx,
                        &self.flags.noise_gate,
                    )
                    .await
                }
                None => Microphone::None,
            },
            MicrophoneSource::Tone { hz, amplitude } => publish_tone(&room, hz, amplitude)
                .await
                .map_or(Microphone::None, |task| Microphone::Tone { _task: task }),
        };
        let published = !matches!(mic, Microphone::None);
        if published && self.flags.microphone_muted.load(Ordering::SeqCst) {
            apply_microphone_mute(&room, true);
        }
        if self.flags.output_muted.load(Ordering::SeqCst) {
            apply_output_mute(&room, true);
        }

        let local = room.local_participant();
        let local_identity = String::from(local.identity());
        let mut participants = vec![Participant {
            name: display_name(local.name(), &local_identity),
            identity: local_identity.clone(),
            muted: self.flags.microphone_muted.load(Ordering::SeqCst),
            speaking: false,
        }];
        let mut remotes: Vec<Participant> = room
            .remote_participants()
            .values()
            .map(remote_view)
            .collect();
        remotes.sort_by(|a, b| a.identity.cmp(&b.identity));
        participants.extend(remotes);
        tracing::info!(
            remote_participants = participants.len() - 1,
            deafened = self.flags.output_muted.load(Ordering::SeqCst),
            microphone_muted = self.flags.microphone_muted.load(Ordering::SeqCst),
            "connected to the room"
        );

        sweep_publications(&room, &tx);
        let mut slot = self.connection.lock().await;
        let events = tokio::spawn(pump_events(
            room_events,
            tx.clone(),
            self.flags.clone(),
            self.watch.clone(),
            self.files.clone(),
        ));
        *slot = Some(Connection {
            room: room.clone(),
            events_tx: tx,
            audio,
            events,
            mic,
        });
        guard.disarm();

        Ok(ConnectedRoom {
            local_identity,
            participants,
            events: rx,
        })
    }
}

impl Drop for LiveKitBackend {
    fn drop(&mut self) {
        self.files.remove_dir();
    }
}

impl Default for LiveKitBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MediaBackend for LiveKitBackend {
    async fn connect(&self, url: &str, token: &str) -> Result<ConnectedRoom, SessionError> {
        timeout(CONNECT_TIMEOUT, self.establish(url, token))
            .await
            .unwrap_or_else(|_| Err(SessionError::Connection("connection timed out".to_owned())))
    }

    async fn disconnect(&self) -> Result<(), SessionError> {
        self.shutdown().await;
        Ok(())
    }

    async fn set_microphone_muted(&self, muted: bool) -> Result<(), SessionError> {
        self.flags.microphone_muted.store(muted, Ordering::SeqCst);
        if let Some(room) = self.current_room().await {
            apply_microphone_mute(&room, muted);
        }
        Ok(())
    }

    async fn set_output_muted(&self, muted: bool) -> Result<(), SessionError> {
        self.flags.output_muted.store(muted, Ordering::SeqCst);
        if let Some(room) = self.current_room().await {
            apply_output_mute(&room, muted);
        }
        Ok(())
    }

    async fn send_data(&self, topic: &str, payload: Vec<u8>) -> Result<(), SessionError> {
        let room = self.current_room().await.ok_or(SessionError::NotInRoom)?;
        let packet = DataPacket {
            payload,
            topic: Some(topic.to_owned()),
            reliable: true,
            destination_identities: Vec::new(),
        };
        room.local_participant()
            .publish_data(packet)
            .await
            .map_err(|err| {
                tracing::warn!(%err, "publishing data failed");
                SessionError::Connection("could not send data".to_owned())
            })
    }

    async fn send_file(&self, transfer: FileTransfer, path: PathBuf) -> Result<(), SessionError> {
        let (room, events) = self
            .current_room_and_events()
            .await
            .ok_or(SessionError::NotInRoom)?;
        livekit_files::start_send(room, events, transfer, path);
        Ok(())
    }

    async fn audio_devices(&self) -> Result<AudioDevices, SessionError> {
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

    async fn set_input_device(&self, id: Option<String>) -> Result<(), SessionError> {
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

    async fn set_output_device(&self, id: Option<String>) -> Result<(), SessionError> {
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

    async fn set_noise_suppression(&self, mode: NoiseSuppression) -> Result<(), SessionError> {
        let previous = self.update_selection(|s| std::mem::replace(&mut s.noise, mode));
        if previous != mode {
            self.republish_microphone().await;
        }
        Ok(())
    }

    async fn set_stream_volume(&self, percent: u8) -> Result<(), SessionError> {
        self.flags
            .stream_attenuation
            .store(100u8.saturating_sub(percent), Ordering::SeqCst);
        Ok(())
    }

    async fn set_noise_gate(&self, threshold_db: Option<i8>) -> Result<(), SessionError> {
        self.flags
            .noise_gate
            .store(threshold_db.unwrap_or(GATE_OFF), Ordering::SeqCst);
        Ok(())
    }

    async fn capture_targets(&self) -> Result<Vec<CaptureTarget>, SessionError> {
        let mut targets = blocking(|| DesktopCaptureSource::new().list_targets()).await??;
        if self.options.test_pattern_target {
            targets.insert(0, test_pattern_target());
        }
        *self.targets.lock().unwrap_or_else(PoisonError::into_inner) = targets.clone();
        Ok(targets)
    }

    async fn start_screen_share(
        &self,
        target_id: &str,
        preset: SharePreset,
    ) -> Result<(), SessionError> {
        let (room, events) = self
            .current_room_and_events()
            .await
            .ok_or(SessionError::NotInRoom)?;
        let mut slot = self.local_share.lock().await;
        if slot.is_some() {
            return Err(SessionError::ShareActive);
        }
        let target = self.resolve_target(target_id).await?;
        let (capture, capture_target) = capture_for(target.clone())?;
        *slot = Some(
            livekit_video::publish_share(room.clone(), events, capture, &capture_target, preset)
                .await?,
        );
        // The picture is what was asked for; the sound is best effort.
        if let Some(scope) = AudioScope::for_target(&target) {
            *self.local_share_audio.lock().await = publish_share_audio(room, scope).await;
        }
        Ok(())
    }

    async fn stop_screen_share(&self) -> Result<(), SessionError> {
        self.stop_local_share().await;
        Ok(())
    }

    async fn watch_screen_share(
        &self,
        identity: &str,
        frames: FrameSlot,
    ) -> Result<(), SessionError> {
        let (room, events) = self
            .current_room_and_events()
            .await
            .ok_or(SessionError::NotInRoom)?;
        let participant = find_participant(&room, identity)
            .ok_or_else(|| SessionError::TargetNotFound(identity.to_owned()))?;
        let publication = participant
            .track_publications()
            .into_values()
            .find(is_screen_share)
            .ok_or_else(|| {
                SessionError::Video("the participant is not sharing their screen".to_owned())
            })?;
        let previous = self.watch.replace(identity, &publication.sid(), frames);
        if let Some(previous) = previous.filter(|previous| previous != identity) {
            set_screen_share_subscribed(&room, &previous, false);
        }
        publication.set_subscribed(true);
        for sound in participant.track_publications().into_values() {
            if is_share_audio(&sound) {
                sound.set_subscribed(true);
            }
        }
        if let Some(RemoteTrack::Video(video)) = publication.track() {
            self.watch.start_stream(identity, video.rtc_track(), events);
        }
        Ok(())
    }

    async fn stop_watching(&self) -> Result<(), SessionError> {
        self.flags.stop_stream();
        if let Some(identity) = self.watch.clear()
            && let Some(room) = self.current_room().await
        {
            set_screen_share_subscribed(&room, &identity, false);
        }
        Ok(())
    }
}

fn capture_for(
    target: CaptureTarget,
) -> Result<(Box<dyn FrameSource>, CaptureTarget), SessionError> {
    if target.id != TEST_PATTERN_TARGET_ID {
        return Ok((Box::new(DesktopCaptureSource::new()), target));
    }
    let (width, height) = TEST_PATTERN_SIZE;
    let mut source = TestPatternSource::new(width, height);
    let inner = source
        .list_targets()?
        .into_iter()
        .next()
        .ok_or_else(|| SessionError::Internal("test pattern has no target".to_owned()))?;
    Ok((Box::new(source), inner))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SessionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| SessionError::Internal(format!("background task failed: {err}")))
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

fn apply_selection(audio: &PlatformAudio, selection: &Selection) {
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

fn install_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn processing_for(mode: NoiseSuppression) -> AudioProcessingOptions {
    AudioProcessingOptions {
        noise_suppression: mode == NoiseSuppression::Standard,
        ..AudioProcessingOptions::default()
    }
}

fn open_audio(mode: NoiseSuppression) -> Option<PlatformAudio> {
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

async fn publish_selected_microphone(
    room: &Room,
    audio: &PlatformAudio,
    selection: &Selection,
    events: &UnboundedSender<BackendEvent>,
    noise_gate: &Arc<AtomicI8>,
) -> Microphone {
    tracing::info!(mode = ?selection.noise, "publishing the microphone");
    if selection.noise.uses_own_capture() {
        if let Some(mic) = publish_enhanced(
            room,
            selection.noise,
            selection.input.clone(),
            events.clone(),
            noise_gate.clone(),
        )
        .await
        {
            return Microphone::Enhanced { _mic: mic };
        }
        tracing::warn!("enhanced microphone unavailable, falling back to the platform one");
    }
    if publish_microphone(room, audio).await {
        Microphone::Platform
    } else {
        Microphone::None
    }
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
    events: UnboundedSender<BackendEvent>,
    noise_gate: Arc<AtomicI8>,
) -> Option<EnhancedMic> {
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
async fn publish_share_audio(room: Arc<Room>, scope: AudioScope) -> Option<ShareAudio> {
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
        sid: publication.sid(),
        _pump: TaskGuard(pump),
        _capture: capture,
    })
}

async fn stop_share_audio(audio: ShareAudio) {
    let ShareAudio { room, sid, .. } = audio;
    if let Err(err) = room.local_participant().unpublish_track(&sid).await {
        tracing::warn!(%err, "unpublishing the share audio failed");
    }
}

async fn publish_microphone(room: &Room, audio: &PlatformAudio) -> bool {
    if audio.recording_devices().next().is_none() {
        tracing::warn!("no microphone found, staying in listen-only mode");
        return false;
    }
    let track = LocalAudioTrack::create_audio_track(MICROPHONE_TRACK_NAME, audio.rtc_source());
    let publish_options = TrackPublishOptions {
        source: TrackSource::Microphone,
        audio_encoding: Some(options::audio::SPEECH.encoding),
        ..Default::default()
    };
    match room
        .local_participant()
        .publish_track(LocalTrack::Audio(track), publish_options)
        .await
    {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!(%err, "could not publish the microphone, staying in listen-only mode");
            false
        }
    }
}

async fn publish_tone(room: &Room, hz: f64, amplitude: f64) -> Option<TaskGuard> {
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
        .publish_track(LocalTrack::Audio(track), publish_options)
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
    Some(TaskGuard(tokio::spawn(tone_loop(source, hz, amplitude))))
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
async fn audio_tap(identity: String, kind: &'static str, mut stream: NativeAudioStream) {
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

fn apply_microphone_mute(room: &Room, muted: bool) {
    for publication in room.local_participant().track_publications().into_values() {
        if publication.source() != TrackSource::Microphone {
            continue;
        }
        if muted {
            publication.mute();
        } else {
            publication.unmute();
        }
    }
}

fn apply_output_mute(room: &Room, muted: bool) {
    for participant in room.remote_participants().into_values() {
        for publication in participant.track_publications().into_values() {
            if publication.kind() == TrackKind::Audio {
                publication.set_enabled(!muted);
            }
        }
    }
}

fn is_share_audio(publication: &RemoteTrackPublication) -> bool {
    publication.kind() == TrackKind::Audio && publication.source() == TrackSource::ScreenshareAudio
}

fn is_screen_share(publication: &RemoteTrackPublication) -> bool {
    publication.kind() == TrackKind::Video && publication.source() == TrackSource::Screenshare
}

fn wants_audio(publication: &RemoteTrackPublication) -> bool {
    publication.kind() == TrackKind::Audio && publication.source() != TrackSource::ScreenshareAudio
}

fn find_participant(room: &Room, identity: &str) -> Option<RemoteParticipant> {
    room.remote_participants()
        .into_values()
        .find(|participant| participant.identity().as_str() == identity)
}

fn set_screen_share_subscribed(room: &Room, identity: &str, subscribed: bool) {
    let Some(participant) = find_participant(room, identity) else {
        return;
    };
    for publication in participant.track_publications().into_values() {
        if is_screen_share(&publication) || is_share_audio(&publication) {
            publication.set_subscribed(subscribed);
        }
    }
}

/// The room is joined with auto-subscribe off: audio is subscribed explicitly here and
/// on every later publication, video only when the user chooses to watch it.
fn sweep_publications(room: &Room, events: &UnboundedSender<BackendEvent>) {
    for participant in room.remote_participants().into_values() {
        let identity = String::from(participant.identity());
        for publication in participant.track_publications().into_values() {
            if wants_audio(&publication) {
                publication.set_subscribed(true);
            } else if is_screen_share(&publication) {
                let _ = events.send(BackendEvent::RemoteScreenShareStarted {
                    identity: identity.clone(),
                });
            }
        }
    }
}

fn microphone_muted(participant: &RemoteParticipant) -> bool {
    participant
        .track_publications()
        .values()
        .any(|publication| {
            publication.source() == TrackSource::Microphone && publication.is_muted()
        })
}

fn remote_view(participant: &RemoteParticipant) -> Participant {
    let identity = String::from(participant.identity());
    Participant {
        name: display_name(participant.name(), &identity),
        muted: microphone_muted(participant),
        speaking: false,
        identity,
    }
}

fn mute_event(
    participant: &SdkParticipant,
    publication: &TrackPublication,
    muted: bool,
) -> Vec<BackendEvent> {
    if publication.source() != TrackSource::Microphone {
        return Vec::new();
    }
    vec![BackendEvent::MuteChanged {
        identity: String::from(participant.identity()),
        muted,
    }]
}

fn translate(
    event: RoomEvent,
    speaking: &mut SpeakingTracker,
    flags: &Flags,
    watch: &Watch,
    events: &UnboundedSender<BackendEvent>,
    files: &FileReceiver,
) -> Vec<BackendEvent> {
    match event {
        RoomEvent::ParticipantConnected(participant) => {
            vec![BackendEvent::ParticipantJoined(remote_view(&participant))]
        }
        RoomEvent::ParticipantDisconnected(participant) => {
            let identity = String::from(participant.identity());
            speaking.forget(&identity);
            watch.clear_if(&identity);
            vec![BackendEvent::ParticipantLeft { identity }]
        }
        RoomEvent::TrackMuted {
            participant,
            publication,
        } => mute_event(&participant, &publication, true),
        RoomEvent::TrackUnmuted {
            participant,
            publication,
        } => mute_event(&participant, &publication, false),
        RoomEvent::TrackPublished {
            participant,
            publication,
        } => {
            let identity = String::from(participant.identity());
            if wants_audio(&publication)
                || (is_share_audio(&publication) && watch.is_watching(&identity))
            {
                publication.set_subscribed(true);
            }
            if is_screen_share(&publication) {
                return vec![BackendEvent::RemoteScreenShareStarted { identity }];
            }
            if publication.source() != TrackSource::Microphone {
                return Vec::new();
            }
            vec![BackendEvent::MuteChanged {
                identity,
                muted: publication.is_muted(),
            }]
        }
        RoomEvent::TrackUnpublished {
            participant,
            publication,
        } => {
            if !is_screen_share(&publication) {
                return Vec::new();
            }
            let identity = String::from(participant.identity());
            watch.clear_if(&identity);
            vec![BackendEvent::RemoteScreenShareStopped { identity }]
        }
        RoomEvent::TrackSubscribed {
            track,
            publication,
            participant,
        } => {
            if let RemoteTrack::Audio(audio) = &track {
                let stream = is_share_audio(&publication);
                if std::env::var_os("ROOMS_AUDIO_TAP").is_some() {
                    tokio::spawn(audio_tap(
                        String::from(participant.identity()),
                        if stream { "stream" } else { "voice" },
                        NativeAudioStream::new(audio.rtc_track(), 48_000, 1),
                    ));
                }
                if stream {
                    flags.play_stream(audio.rtc_track());
                }
                let deafened = flags.output_muted.load(Ordering::SeqCst);
                if deafened {
                    publication.set_enabled(false);
                }
                tracing::info!(
                    identity = %String::from(participant.identity()),
                    deafened,
                    track_enabled = publication.is_enabled(),
                    "remote audio track subscribed"
                );
            } else if let RemoteTrack::Video(video) = &track
                && is_screen_share(&publication)
            {
                let identity = String::from(participant.identity());
                tracing::info!(%identity, "remote screen share track subscribed");
                watch.start_stream(&identity, video.rtc_track(), events.clone());
            }
            Vec::new()
        }
        RoomEvent::TrackUnsubscribed {
            track,
            participant,
            publication,
        } => {
            if is_share_audio(&publication) {
                flags.stop_stream();
            }
            if matches!(track, RemoteTrack::Audio(_)) {
                tracing::info!(
                    identity = %String::from(participant.identity()),
                    deafened = flags.output_muted.load(Ordering::SeqCst),
                    "remote audio track unsubscribed"
                );
            }
            Vec::new()
        }
        RoomEvent::TrackSubscriptionFailed {
            participant,
            error,
            track_sid,
        } => {
            let identity = String::from(participant.identity());
            tracing::warn!(%identity, ?error, "remote track subscription failed");
            if watch.failed_sid_matches(&identity, &track_sid) {
                watch.clear();
                return vec![BackendEvent::WatchEnded {
                    identity,
                    reason: Some("could not subscribe to the screen share".to_owned()),
                }];
            }
            Vec::new()
        }
        RoomEvent::LocalTrackRepublished {
            track: LocalTrack::Audio(track),
            ..
        } => {
            if flags.microphone_muted.load(Ordering::SeqCst) {
                track.disable();
            }
            Vec::new()
        }
        RoomEvent::ActiveSpeakersChanged { speakers } => {
            let now: HashSet<String> = speakers
                .iter()
                .map(|speaker| String::from(speaker.identity()))
                .collect();
            speaking.update(now)
        }
        RoomEvent::DataReceived {
            payload,
            topic,
            kind,
            participant,
        } => match (kind, topic, participant) {
            (DataPacketKind::Reliable, Some(topic), Some(sender)) => {
                vec![BackendEvent::DataReceived {
                    sender: String::from(sender.identity()),
                    topic,
                    payload: Arc::unwrap_or_clone(payload),
                }]
            }
            _ => Vec::new(),
        },
        RoomEvent::ByteStreamOpened {
            reader,
            topic,
            participant_identity,
        } => {
            if topic == FILE_TOPIC
                && let Some(reader) = reader.take()
            {
                livekit_files::start_receive(
                    reader,
                    String::from(participant_identity),
                    files,
                    events,
                );
            }
            Vec::new()
        }
        RoomEvent::Reconnecting => vec![BackendEvent::Reconnecting],
        RoomEvent::Reconnected => vec![BackendEvent::Reconnected],
        RoomEvent::Disconnected { reason } => vec![BackendEvent::Disconnected {
            reason: format!("{reason:?}"),
        }],
        _ => Vec::new(),
    }
}

async fn pump_events(
    mut room_events: UnboundedReceiver<RoomEvent>,
    tx: UnboundedSender<BackendEvent>,
    flags: Arc<Flags>,
    watch: Arc<Watch>,
    files: Arc<FileReceiver>,
) {
    let mut speaking = SpeakingTracker::default();
    while let Some(event) = room_events.recv().await {
        let mut finished = false;
        for backend_event in translate(event, &mut speaking, &flags, &watch, &tx, &files) {
            finished |= matches!(backend_event, BackendEvent::Disconnected { .. });
            if tx.send(backend_event).is_err() {
                return;
            }
        }
        if finished {
            return;
        }
    }
    let _ = tx.send(BackendEvent::Disconnected {
        reason: "room event stream ended".to_owned(),
    });
}
