//! Real media backend using the LiveKit SDK.

mod audio;
mod data;
mod events;
mod files;
mod room;
mod stream_player;
mod video;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use livekit::prelude::TrackSid;
use livekit::webrtc::audio_track::RtcAudioTrack;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::api::{AudioDevices, FileTransfer, NoiseSuppression, SessionError};
use crate::audio::audio_render::{RENDER_SAMPLE_RATE, play_once};
use crate::audio::cue::{Cue, cue_samples};
use crate::audio::participant_volume::ParticipantVolumes;
use crate::backend::options::LiveKitBackendOptions;
use crate::backend::support::GATE_OFF;
use crate::backend::{ConnectedRoom, MediaBackend};
use crate::files::receive::FileReceiver;
use crate::livekit_backend::video::{LocalShare, Watch};
use crate::video::{CaptureTarget, FrameSlot, SharePreset, VideoFrame};

use self::audio::{Selection, ShareAudio};
use self::room::Connection;
use self::stream_player::StreamPlayer;

struct TaskGuard(JoinHandle<()>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug)]
struct UiVisibleFlag(Arc<AtomicBool>);

impl Default for UiVisibleFlag {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}

#[derive(Debug, Default)]
struct ShareSids {
    video: StdMutex<Option<std::sync::Weak<StdMutex<TrackSid>>>>,
    audio: StdMutex<Option<std::sync::Weak<StdMutex<TrackSid>>>>,
}

impl ShareSids {
    fn set_video(&self, sid: &Arc<StdMutex<TrackSid>>) {
        Self::set(&self.video, sid);
    }

    fn set_audio(&self, sid: &Arc<StdMutex<TrackSid>>) {
        Self::set(&self.audio, sid);
    }

    fn republish_video(&self, previous: &TrackSid, next: TrackSid) -> bool {
        Self::update(&self.video, previous, next)
    }

    fn republish_audio(&self, previous: &TrackSid, next: TrackSid) -> bool {
        Self::update(&self.audio, previous, next)
    }

    fn set(
        slot: &StdMutex<Option<std::sync::Weak<StdMutex<TrackSid>>>>,
        sid: &Arc<StdMutex<TrackSid>>,
    ) {
        let mut guard = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(Arc::downgrade(sid));
    }

    fn update(
        slot: &StdMutex<Option<std::sync::Weak<StdMutex<TrackSid>>>>,
        previous: &TrackSid,
        next: TrackSid,
    ) -> bool {
        let target = {
            let mut guard = slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match guard.as_ref().and_then(std::sync::Weak::upgrade) {
                Some(arc) => arc,
                None => {
                    *guard = None;
                    return false;
                }
            }
        };
        let mut sid = target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *sid == *previous {
            *sid = next;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Default)]
struct Flags {
    microphone_muted: AtomicBool,
    output_muted: AtomicBool,
    /// Echo cancellation of the own capture, against what this app plays.
    echo_cancellation: AtomicBool,
    ui_visible: UiVisibleFlag,
    share_sids: ShareSids,
    /// 100 minus the stream volume, so the zero default means full volume.
    stream_attenuation: Arc<AtomicU8>,
    /// Noise gate threshold in dBFS; the zero default means the gate is off.
    noise_gate: Arc<AtomicI8>,
    /// Output device of the stream sound; `None` is the Windows default.
    stream_output: StdMutex<Option<String>>,
    /// Sound of the screen share being watched.
    stream_player: StdMutex<Option<StreamPlayer>>,
    /// Per-participant voice gain overrides (`0..=200`, `100` omitted).
    participant_volumes: StdMutex<ParticipantVolumes>,
    /// Set when the voice engine cannot open the output device: voices then play
    /// through the app's own output, keyed by track sid.
    own_voice_output: AtomicBool,
    voice_players: StdMutex<HashMap<String, VoicePlayer>>,
}

#[derive(Debug)]
struct VoicePlayer {
    identity: String,
    /// `f32` bits of the participant's linear gain.
    gain: Arc<AtomicU32>,
    player: StreamPlayer,
}

impl VoicePlayer {
    fn start(
        identity: String,
        gain: Arc<AtomicU32>,
        track: RtcAudioTrack,
        device: Option<String>,
    ) -> Self {
        let level = gain.clone();
        let player = StreamPlayer::start(track, device, move || {
            f32::from_bits(level.load(Ordering::Relaxed)) / 32768.0
        });
        Self {
            identity,
            gain,
            player,
        }
    }
}

impl Flags {
    fn stream_output(&self) -> Option<String> {
        self.stream_output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Plays `track` through the app's own output so its volume can be set apart from voices.
    fn play_stream(&self, track: RtcAudioTrack) {
        // The engine keeps decoding the track but no longer plays it.
        track.set_enabled(false);
        let attenuation = self.stream_attenuation.clone();
        let player = StreamPlayer::start(track, self.stream_output(), move || {
            let volume =
                f32::from(100u8.saturating_sub(attenuation.load(Ordering::SeqCst))) / 100.0;
            // Squared so the slider follows how loudness is heard.
            volume * volume / 32768.0
        });
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
        let device = self.stream_output();
        let mut players = self
            .voice_players
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let moved: Vec<(String, VoicePlayer)> = players.drain().collect();
        for (sid, voice) in moved {
            let VoicePlayer {
                identity,
                gain,
                player,
            } = voice;
            let track = player.track.clone();
            drop(player);
            players.insert(
                sid,
                VoicePlayer::start(identity, gain, track, device.clone()),
            );
        }
    }

    /// Volume goes to the engine, or to the app's own output when that one plays the voices.
    fn attach_voice(&self, identity: &str, sid: String, track: RtcAudioTrack) {
        if !self.own_voice_output.load(Ordering::SeqCst) {
            self.apply_participant_volume(identity, &track);
            return;
        }
        let gain = self
            .participant_volumes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .gain_for(identity)
            .unwrap_or(1.0) as f32;
        let voice = VoicePlayer::start(
            identity.to_owned(),
            Arc::new(AtomicU32::new(gain.to_bits())),
            track,
            self.stream_output(),
        );
        self.voice_players
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(sid, voice);
    }

    fn detach_voice(&self, sid: &str) {
        self.voice_players
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(sid);
    }

    fn stop_voices(&self) {
        self.own_voice_output.store(false, Ordering::SeqCst);
        self.voice_players
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn set_voice_gain(&self, identity: &str, gain: f32) {
        for voice in self
            .voice_players
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|voice| voice.identity == identity)
        {
            voice.gain.store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    fn set_participant_volume(&self, identity: &str, percent: u16) -> u16 {
        self.participant_volumes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set(identity, percent)
    }

    fn apply_participant_volume(&self, identity: &str, track: &RtcAudioTrack) {
        let gain = self
            .participant_volumes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .gain_for(identity);
        if let Some(gain) = gain {
            track.set_volume(gain);
        }
    }
}

/// Real media backend: LiveKit room, microphone and playback through the platform audio.
pub struct LiveKitBackend {
    connection: Arc<Mutex<Option<Connection>>>,
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
            connection: Arc::new(Mutex::new(None)),
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
        self.connect_room(url, token).await
    }

    async fn disconnect(&self) -> Result<(), SessionError> {
        self.shutdown().await;
        Ok(())
    }

    async fn set_microphone_muted(&self, muted: bool) -> Result<(), SessionError> {
        self.flags.microphone_muted.store(muted, Ordering::SeqCst);
        if let Some(room) = self.current_room().await {
            room::apply_microphone_mute(&room, muted);
        }
        Ok(())
    }

    async fn set_output_muted(&self, muted: bool) -> Result<(), SessionError> {
        self.flags.output_muted.store(muted, Ordering::SeqCst);
        if let Some(room) = self.current_room().await {
            room::apply_output_mute(&room, muted);
            self.publish_deafened_attribute(&room, muted).await;
        }
        Ok(())
    }

    async fn set_display_name(&self, name: &str) -> Result<(), SessionError> {
        self.publish_name(name).await
    }

    async fn set_avatar_attribute(&self, version: Option<&str>) -> Result<(), SessionError> {
        self.publish_avatar_attribute(version).await
    }

    async fn send_data(&self, topic: &str, payload: Vec<u8>) -> Result<(), SessionError> {
        self.publish_data(topic, payload).await
    }

    async fn send_file(&self, transfer: FileTransfer, path: PathBuf) -> Result<(), SessionError> {
        self.start_file_send(transfer, path).await
    }

    async fn audio_devices(&self) -> Result<AudioDevices, SessionError> {
        self.list_audio_devices().await
    }

    async fn set_input_device(&self, id: Option<String>) -> Result<(), SessionError> {
        self.select_input_device(id).await
    }

    async fn set_output_device(&self, id: Option<String>) -> Result<(), SessionError> {
        self.select_output_device(id).await
    }

    async fn set_noise_suppression(&self, mode: NoiseSuppression) -> Result<(), SessionError> {
        self.select_noise_suppression(mode).await
    }

    async fn set_echo_cancellation(&self, enabled: bool) -> Result<(), SessionError> {
        let previous = self.flags.echo_cancellation.swap(enabled, Ordering::SeqCst);
        if previous != enabled && self.selection().noise.uses_own_capture() {
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

    async fn set_participant_volume(
        &self,
        identity: &str,
        percent: u16,
    ) -> Result<(), SessionError> {
        self.update_participant_volume(identity, percent).await
    }

    async fn set_noise_gate(&self, threshold_db: Option<i8>) -> Result<(), SessionError> {
        self.flags
            .noise_gate
            .store(threshold_db.unwrap_or(GATE_OFF), Ordering::SeqCst);
        Ok(())
    }

    async fn capture_targets(&self) -> Result<Vec<CaptureTarget>, SessionError> {
        self.list_capture_targets().await
    }

    async fn capture_thumbnail(
        &self,
        target: &CaptureTarget,
        max_side: u32,
        timeout: Duration,
    ) -> Result<VideoFrame, SessionError> {
        self.capture_target_thumbnail(target, max_side, timeout)
            .await
    }

    async fn start_screen_share(
        &self,
        target_id: &str,
        preset: SharePreset,
        preview: FrameSlot,
    ) -> Result<(), SessionError> {
        self.start_local_screen_share(target_id, preset, preview)
            .await
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
        self.watch_remote_screen_share(identity, frames).await
    }

    async fn stop_watching(&self) -> Result<(), SessionError> {
        self.stop_watching_share().await
    }

    fn set_ui_visible(&self, visible: bool) {
        let previous = self.flags.ui_visible.0.swap(visible, Ordering::SeqCst);
        if previous == visible {
            return;
        }
        if !visible {
            self.watch.pause_stream();
        }
        if self.watch.watched_identity().is_none() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let connection = Arc::clone(&self.connection);
        let watch = Arc::clone(&self.watch);
        let flags = Arc::clone(&self.flags);
        handle.spawn(async move {
            let current = connection
                .lock()
                .await
                .as_ref()
                .map(|conn| (conn.room.clone(), conn.events_tx.clone()));
            let Some((room, events)) = current else {
                return;
            };
            let Some(identity) = watch.watched_identity() else {
                return;
            };
            let visible = flags.ui_visible.0.load(Ordering::SeqCst);
            let track = room::set_screen_share_video_subscribed(&room, &identity, visible);
            if visible && let Some(video) = track {
                watch.start_stream(&identity, video, events, Arc::clone(&flags.ui_visible.0));
            }
        });
    }

    fn play_cue(&self, cue: Cue) {
        play_once(
            self.flags.stream_output(),
            cue_samples(cue, RENDER_SAMPLE_RATE),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_sid(id: &str) -> TrackSid {
        TrackSid::try_from(id.to_owned()).expect("valid track sid")
    }

    #[test]
    fn share_sids_update_when_previous_matches_and_ignore_stale_tracks() {
        let sids = ShareSids::default();
        let video_sid = Arc::new(StdMutex::new(track_sid("TR_video_1")));
        let audio_sid = Arc::new(StdMutex::new(track_sid("TR_audio_1")));
        sids.set_video(&video_sid);
        sids.set_audio(&audio_sid);

        assert!(!sids.republish_video(&track_sid("TR_other"), track_sid("TR_video_x")));
        assert_eq!(*video_sid.lock().unwrap(), track_sid("TR_video_1"));

        assert!(sids.republish_video(&track_sid("TR_video_1"), track_sid("TR_video_2")));
        assert_eq!(*video_sid.lock().unwrap(), track_sid("TR_video_2"));

        assert!(sids.republish_video(&track_sid("TR_video_2"), track_sid("TR_video_3")));
        assert_eq!(*video_sid.lock().unwrap(), track_sid("TR_video_3"));

        assert!(sids.republish_audio(&track_sid("TR_audio_1"), track_sid("TR_audio_2")));
        assert_eq!(*audio_sid.lock().unwrap(), track_sid("TR_audio_2"));

        drop(video_sid);
        assert!(!sids.republish_video(&track_sid("TR_video_3"), track_sid("TR_video_4")));
    }
}
