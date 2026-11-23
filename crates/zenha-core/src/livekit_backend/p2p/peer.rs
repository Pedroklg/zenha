//! Wrapper `P2pPeer` sobre `PeerConnection` da `libwebrtc` para envio e recepção P2P.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use livekit::rtc_engine::lk_runtime::LkRuntime;
use livekit::webrtc::ice_candidate::IceCandidate;
use livekit::webrtc::media_stream_track::MediaStreamTrack;
use livekit::webrtc::peer_connection::{
    AnswerOptions, BitrateSettings, OfferOptions, PeerConnection,
};
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use livekit::webrtc::rtp_parameters::RtpEncodingParameters;
use livekit::webrtc::rtp_sender::VideoEncoderBackend;
use livekit::webrtc::rtp_transceiver::{
    RtpTransceiver, RtpTransceiverDirection, RtpTransceiverInit,
};
use livekit::webrtc::session_description::{SdpParseError, SdpType, SessionDescription};
use livekit::webrtc::video_frame::{EncodedVideoFrame, VideoBufferType, VideoFormatType};
use livekit::webrtc::video_source::EncodedRateControl;
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use livekit::webrtc::video_track::RtcVideoTrack;
use livekit::webrtc::{MediaType, RtcError};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use zenha_protocol::P2pSignal;

use crate::video::FrameSlot;

use super::{
    IceBufferAction, IceCandidateBuffer, IceCandidateData, P2pPeerConfig, P2pPeerEvent,
    P2pPeerState, P2pRole, P2pStatsSnapshot, build_rtc_configuration, extract_stats_snapshot,
    prioritize_h264_codecs,
};

const P2P_VIDEO_TRACK_LABEL: &str = "p2p-screen";
const P2P_VIDEO_STREAM_ID: &str = "p2p-screen-stream";

#[derive(Debug, thiserror::Error)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) enum P2pPeerError {
    #[error("webrtc error: {0}")]
    Rtc(#[from] RtcError),
    #[error("sdp parse error: {0}")]
    Sdp(#[from] SdpParseError),
    #[error("p2p peer is closed")]
    Closed,
    #[error("p2p peer role does not support this operation")]
    WrongRole,
    #[error("ice candidate limit reached for session")]
    IceCandidateLimitReached,
}

/// Encapsulates a single WebRTC `PeerConnection` for one P2P screen-share link.
///
/// Field drop order matters: background tasks, tracks and the `PeerConnection` drop before
/// `_lk_runtime`, and `Drop` explicitly closes the connection (`CORE-07`).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) struct P2pPeer {
    role: P2pRole,
    session_id: String,
    closed: Arc<AtomicBool>,
    stats_task: StdMutex<Option<JoinHandle<()>>>,
    receive_task: StdMutex<Option<JoinHandle<()>>>,
    ice_buffer: StdMutex<IceCandidateBuffer>,
    video_source: Option<NativeVideoSource>,
    _transceiver: Option<RtpTransceiver>,
    pc: PeerConnection,
    _lk_runtime: Arc<LkRuntime>,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
impl P2pPeer {
    pub(crate) fn new_sender(
        config: P2pPeerConfig,
    ) -> Result<(Self, UnboundedReceiver<P2pPeerEvent>), P2pPeerError> {
        let lk_runtime = LkRuntime::instance();
        let factory = lk_runtime.pc_factory();
        let pc = factory.create_peer_connection(build_rtc_configuration(&config.stun_urls))?;

        let source = NativeVideoSource::new_encoded(config.video_resolution);
        let track = factory.create_video_track(P2P_VIDEO_TRACK_LABEL, source.clone());
        let init = RtpTransceiverInit {
            direction: RtpTransceiverDirection::SendOnly,
            stream_ids: vec![P2P_VIDEO_STREAM_ID.to_owned()],
            send_encodings: vec![RtpEncodingParameters {
                max_bitrate: config.bitrate.max_bitrate_bps,
                max_framerate: config.max_framerate,
                ..Default::default()
            }],
        };
        let transceiver = pc.add_transceiver(MediaStreamTrack::Video(track), init)?;
        transceiver
            .sender()
            .set_video_encoder_backend(VideoEncoderBackend::PreEncoded);

        let capabilities = factory.get_rtp_sender_capabilities(MediaType::Video).codecs;
        let preferred = prioritize_h264_codecs(capabilities);
        if !preferred.is_empty() {
            transceiver.set_codec_preferences(preferred)?;
        }

        if has_bitrate_setting(config.bitrate) {
            pc.set_bitrate(config.bitrate)?;
        }

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        wire_common_callbacks(&pc, &config.session_id, &closed, &events_tx);
        let stats_task = spawn_stats_task(
            pc.clone(),
            Arc::clone(&closed),
            events_tx,
            config.stats_interval,
        );

        Ok((
            Self {
                role: P2pRole::Sender,
                session_id: config.session_id,
                closed,
                stats_task: StdMutex::new(stats_task),
                receive_task: StdMutex::new(None),
                ice_buffer: StdMutex::new(IceCandidateBuffer::default()),
                video_source: Some(source),
                _transceiver: Some(transceiver),
                pc,
                _lk_runtime: lk_runtime,
            },
            events_rx,
        ))
    }

    pub(crate) fn new_receiver(
        config: P2pPeerConfig,
    ) -> Result<(Self, UnboundedReceiver<P2pPeerEvent>), P2pPeerError> {
        let lk_runtime = LkRuntime::instance();
        let factory = lk_runtime.pc_factory();
        let pc = factory.create_peer_connection(build_rtc_configuration(&config.stun_urls))?;

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        wire_common_callbacks(&pc, &config.session_id, &closed, &events_tx);

        let track_closed = Arc::clone(&closed);
        let track_tx = events_tx.clone();
        pc.on_track(Some(Box::new(move |event| {
            if track_closed.load(Ordering::Acquire) {
                return;
            }
            if let MediaStreamTrack::Video(video) = event.track {
                let _ = track_tx.send(P2pPeerEvent::VideoTrack(video));
            }
        })));

        let stats_task = spawn_stats_task(
            pc.clone(),
            Arc::clone(&closed),
            events_tx,
            config.stats_interval,
        );

        Ok((
            Self {
                role: P2pRole::Receiver,
                session_id: config.session_id,
                closed,
                stats_task: StdMutex::new(stats_task),
                receive_task: StdMutex::new(None),
                ice_buffer: StdMutex::new(IceCandidateBuffer::default()),
                video_source: None,
                _transceiver: None,
                pc,
                _lk_runtime: lk_runtime,
            },
            events_rx,
        ))
    }

    pub(crate) fn role(&self) -> P2pRole {
        self.role
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn state(&self) -> P2pPeerState {
        if self.closed.load(Ordering::Acquire) {
            return P2pPeerState::Closed;
        }
        P2pPeerState::from_rtc(self.pc.connection_state())
    }

    pub(crate) fn video_source(&self) -> Option<&NativeVideoSource> {
        self.video_source.as_ref()
    }

    pub(crate) fn push_encoded_frame(&self, frame: &EncodedVideoFrame<'_>) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.video_source
            .as_ref()
            .is_some_and(|source| source.capture_encoded_frame(frame))
    }

    pub(crate) fn take_keyframe_request(&self) -> bool {
        self.video_source
            .as_ref()
            .is_some_and(NativeVideoSource::take_keyframe_request)
    }

    pub(crate) fn take_rate_control_request(&self) -> Option<EncodedRateControl> {
        self.video_source
            .as_ref()
            .and_then(NativeVideoSource::take_rate_control_request)
    }

    pub(crate) fn set_bitrate(&self, settings: BitrateSettings) -> Result<(), P2pPeerError> {
        self.ensure_open()?;
        self.pc.set_bitrate(settings)?;
        Ok(())
    }

    pub(crate) async fn create_offer(&self, share_epoch: u64) -> Result<P2pSignal, P2pPeerError> {
        self.ensure_open()?;
        if self.role != P2pRole::Sender {
            return Err(P2pPeerError::WrongRole);
        }
        let offer = self.pc.create_offer(OfferOptions::default()).await?;
        let sdp = offer.to_string();
        self.pc.set_local_description(offer).await?;
        Ok(P2pSignal::Offer {
            share_epoch,
            session_id: self.session_id.clone(),
            sdp,
        })
    }

    pub(crate) async fn accept_offer(&self, sdp: &str) -> Result<P2pSignal, P2pPeerError> {
        self.ensure_open()?;
        if self.role != P2pRole::Receiver {
            return Err(P2pPeerError::WrongRole);
        }
        let remote = SessionDescription::parse(sdp, SdpType::Offer)?;
        self.pc.set_remote_description(remote).await?;
        self.flush_buffered_ice_candidates().await;
        let answer = self.pc.create_answer(AnswerOptions::default()).await?;
        let answer_sdp = answer.to_string();
        self.pc.set_local_description(answer).await?;
        Ok(P2pSignal::Answer {
            session_id: self.session_id.clone(),
            sdp: answer_sdp,
        })
    }

    pub(crate) async fn accept_answer(&self, sdp: &str) -> Result<(), P2pPeerError> {
        self.ensure_open()?;
        if self.role != P2pRole::Sender {
            return Err(P2pPeerError::WrongRole);
        }
        let remote = SessionDescription::parse(sdp, SdpType::Answer)?;
        self.pc.set_remote_description(remote).await?;
        self.flush_buffered_ice_candidates().await;
        Ok(())
    }

    pub(crate) async fn add_ice_candidate(
        &self,
        candidate: &str,
        sdp_mid: &str,
        sdp_mline_index: i32,
    ) -> Result<(), P2pPeerError> {
        self.ensure_open()?;
        let action = self
            .ice_buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(IceCandidateData {
                candidate: candidate.to_owned(),
                sdp_mid: sdp_mid.to_owned(),
                sdp_mline_index,
            });
        match action {
            IceBufferAction::ApplyNow(data) => self.apply_ice_candidate(data).await,
            IceBufferAction::Buffered => Ok(()),
            IceBufferAction::LimitReached => {
                tracing::warn!(
                    session_id = %self.session_id,
                    "p2p ice candidate limit reached"
                );
                Err(P2pPeerError::IceCandidateLimitReached)
            }
        }
    }

    pub(crate) async fn stats(&self) -> Result<P2pStatsSnapshot, P2pPeerError> {
        self.ensure_open()?;
        let raw = self.pc.get_stats().await?;
        Ok(extract_stats_snapshot(&raw, None))
    }

    /// Streams decoded frames from `track` into `slot` until the track ends or the peer closes.
    pub(crate) fn attach_frame_slot(&self, track: RtcVideoTrack, slot: FrameSlot) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let closed = Arc::clone(&self.closed);
        let task = handle.spawn(read_decoded_frames(track, slot, closed));
        if let Some(previous) = self
            .receive_task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(task)
        {
            previous.abort();
        }
    }

    pub(crate) fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(task) = self
            .stats_task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        if let Some(task) = self
            .receive_task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        self.pc.on_ice_candidate(None);
        self.pc.on_ice_candidate_error(None);
        self.pc.on_connection_state_change(None);
        self.pc.on_ice_connection_state_change(None);
        self.pc.on_ice_gathering_state_change(None);
        self.pc.on_signaling_state_change(None);
        self.pc.on_track(None);
        self.pc.on_data_channel(None);
        self.pc.on_negotiation_needed(None);
        self.pc.close();
    }

    fn ensure_open(&self) -> Result<(), P2pPeerError> {
        if self.closed.load(Ordering::Acquire) {
            Err(P2pPeerError::Closed)
        } else {
            Ok(())
        }
    }

    async fn flush_buffered_ice_candidates(&self) {
        let pending = self
            .ice_buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mark_remote_description_set();
        for candidate in pending {
            if let Err(err) = self.apply_ice_candidate(candidate).await {
                tracing::debug!(
                    session_id = %self.session_id,
                    %err,
                    "applying buffered p2p ice candidate failed"
                );
            }
        }
    }

    async fn apply_ice_candidate(&self, data: IceCandidateData) -> Result<(), P2pPeerError> {
        let parsed = IceCandidate::parse(&data.sdp_mid, data.sdp_mline_index, &data.candidate)?;
        self.pc.add_ice_candidate(parsed).await?;
        Ok(())
    }
}

impl Drop for P2pPeer {
    fn drop(&mut self) {
        self.close();
    }
}

fn has_bitrate_setting(settings: BitrateSettings) -> bool {
    settings.min_bitrate_bps.is_some()
        || settings.start_bitrate_bps.is_some()
        || settings.max_bitrate_bps.is_some()
}

fn wire_common_callbacks(
    pc: &PeerConnection,
    session_id: &str,
    closed: &Arc<AtomicBool>,
    events_tx: &UnboundedSender<P2pPeerEvent>,
) {
    let ice_closed = Arc::clone(closed);
    let ice_tx = events_tx.clone();
    let ice_session_id = session_id.to_owned();
    pc.on_ice_candidate(Some(Box::new(move |candidate| {
        if ice_closed.load(Ordering::Acquire) {
            return;
        }
        let signal = P2pSignal::Ice {
            session_id: ice_session_id.clone(),
            candidate: candidate.candidate(),
            sdp_mid: candidate.sdp_mid(),
            sdp_mline_index: candidate.sdp_mline_index(),
        };
        let _ = ice_tx.send(P2pPeerEvent::IceCandidate(signal));
    })));

    let state_closed = Arc::clone(closed);
    let state_tx = events_tx.clone();
    pc.on_connection_state_change(Some(Box::new(move |state| {
        if state_closed.load(Ordering::Acquire) {
            return;
        }
        let _ = state_tx.send(P2pPeerEvent::StateChanged(P2pPeerState::from_rtc(state)));
    })));
}

fn spawn_stats_task(
    pc: PeerConnection,
    closed: Arc<AtomicBool>,
    events_tx: UnboundedSender<P2pPeerEvent>,
    interval: Duration,
) -> Option<JoinHandle<()>> {
    if interval.is_zero() {
        return None;
    }
    let handle = tokio::runtime::Handle::try_current().ok()?;
    Some(handle.spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        let mut previous: Option<P2pStatsSnapshot> = None;
        while !closed.load(Ordering::Acquire) {
            ticker.tick().await;
            if closed.load(Ordering::Acquire) {
                break;
            }
            let Ok(raw) = pc.get_stats().await else {
                continue;
            };
            let snapshot = extract_stats_snapshot(&raw, previous.as_ref());
            previous = Some(snapshot.clone());
            if events_tx.send(P2pPeerEvent::Stats(snapshot)).is_err() {
                break;
            }
        }
    }))
}

async fn read_decoded_frames(track: RtcVideoTrack, slot: FrameSlot, closed: Arc<AtomicBool>) {
    let mut stream = NativeVideoStream::new(track);
    while let Some(frame) = stream.next().await {
        if closed.load(Ordering::Acquire) {
            break;
        }
        let buffer = &frame.buffer;
        if buffer.buffer_type() == VideoBufferType::Native {
            tracing::warn!("received native video buffer on p2p track that cannot be read on CPU");
            continue;
        }
        let (width, height) = (buffer.width(), buffer.height());
        let (Ok(w), Ok(h)) = (i32::try_from(width), i32::try_from(height)) else {
            continue;
        };
        if width == 0 || height == 0 {
            continue;
        }
        let converted = catch_unwind(AssertUnwindSafe(|| {
            slot.push_with(width, height, |rgba| {
                buffer.to_argb(VideoFormatType::ABGR, rgba, width * 4, w, h);
            });
        }));
        if converted.is_err() {
            tracing::warn!("converting a received p2p frame failed, frame skipped");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::livekit_backend::p2p_signal::{decode_p2p_signal, encode_p2p_signal};
    use livekit::webrtc::video_frame::{EncodedFrameType, EncodedVideoCodec};
    use livekit::webrtc::video_source::VideoResolution;
    use tokio::time::timeout;

    use super::*;

    struct BitWriter {
        bytes: Vec<u8>,
        current: u8,
        bits_in_current: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                bits_in_current: 0,
            }
        }

        fn write_bit(&mut self, bit: u8) {
            self.current = (self.current << 1) | (bit & 1);
            self.bits_in_current += 1;
            if self.bits_in_current == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.bits_in_current = 0;
            }
        }

        fn write_bits(&mut self, value: u32, count: u8) {
            for shift in (0..count).rev() {
                self.write_bit(((value >> shift) & 1) as u8);
            }
        }

        fn write_ue(&mut self, value: u32) {
            let code_num = value + 1;
            let bits = (32 - code_num.leading_zeros()) as u8;
            for _ in 0..(bits - 1) {
                self.write_bit(0);
            }
            self.write_bits(code_num, bits);
        }

        fn write_se(&mut self, value: i32) {
            let mapped = if value <= 0 {
                (-2 * value) as u32
            } else {
                (2 * value - 1) as u32
            };
            self.write_ue(mapped);
        }

        fn align_zero(&mut self) {
            while self.bits_in_current != 0 {
                self.write_bit(0);
            }
        }

        fn write_byte_aligned(&mut self, byte: u8) {
            assert_eq!(self.bits_in_current, 0);
            self.bytes.push(byte);
        }

        fn finish_rbsp(mut self) -> Vec<u8> {
            self.write_bit(1);
            self.align_zero();
            self.bytes
        }
    }

    fn append_annex_b_nal(out: &mut Vec<u8>, nal_header: u8, rbsp: &[u8]) {
        out.extend_from_slice(&[0, 0, 0, 1, nal_header]);
        let mut zeros = 0u8;
        for &byte in rbsp {
            if zeros == 2 && byte <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(byte);
            if byte == 0 {
                zeros = zeros.saturating_add(1);
            } else {
                zeros = 0;
            }
        }
    }

    /// Generates a valid Constrained Baseline Annex-B H.264 IDR keyframe using I_PCM macroblocks.
    fn synthetic_h264_keyframe(
        width: u32,
        height: u32,
        idr_pic_id: u32,
        y: u8,
        cb: u8,
        cr: u8,
    ) -> Vec<u8> {
        assert_eq!(width % 16, 0);
        assert_eq!(height % 16, 0);
        let mb_w = width / 16;
        let mb_h = height / 16;

        let mut sps = BitWriter::new();
        sps.write_bits(66, 8);
        sps.write_bits(0xE0, 8);
        sps.write_bits(31, 8);
        sps.write_ue(0);
        sps.write_ue(0);
        sps.write_ue(2);
        sps.write_ue(1);
        sps.write_bit(0);
        sps.write_ue(mb_w - 1);
        sps.write_ue(mb_h - 1);
        sps.write_bit(1);
        sps.write_bit(0);
        sps.write_bit(0);
        sps.write_bit(0);
        let sps_rbsp = sps.finish_rbsp();

        let mut pps = BitWriter::new();
        pps.write_ue(0);
        pps.write_ue(0);
        pps.write_bit(0);
        pps.write_bit(0);
        pps.write_ue(0);
        pps.write_ue(0);
        pps.write_ue(0);
        pps.write_bit(0);
        pps.write_bits(0, 2);
        pps.write_se(0);
        pps.write_se(0);
        pps.write_se(0);
        pps.write_bit(0);
        pps.write_bit(0);
        pps.write_bit(0);
        let pps_rbsp = pps.finish_rbsp();

        let mut slice = BitWriter::new();
        slice.write_ue(0);
        slice.write_ue(7);
        slice.write_ue(0);
        slice.write_bits(0, 4);
        slice.write_ue(idr_pic_id);
        slice.write_bit(0);
        slice.write_bit(0);
        slice.write_se(0);
        for _ in 0..(mb_w * mb_h) {
            slice.write_ue(25);
            slice.align_zero();
            for _ in 0..256 {
                slice.write_byte_aligned(y);
            }
            for _ in 0..64 {
                slice.write_byte_aligned(cb);
            }
            for _ in 0..64 {
                slice.write_byte_aligned(cr);
            }
        }
        let slice_rbsp = slice.finish_rbsp();

        let mut annex_b = Vec::new();
        append_annex_b_nal(&mut annex_b, 0x67, &sps_rbsp);
        append_annex_b_nal(&mut annex_b, 0x68, &pps_rbsp);
        append_annex_b_nal(&mut annex_b, 0x65, &slice_rbsp);
        annex_b
    }

    fn round_trip_signal(signal: P2pSignal) -> P2pSignal {
        let bytes = encode_p2p_signal(&signal).unwrap();
        decode_p2p_signal(&bytes).unwrap()
    }

    #[tokio::test]
    async fn loopback_p2p_delivers_decoded_h264_frames_and_stats() {
        let width = 64;
        let height = 64;
        let session_id = "loopback-sess-1";

        let mut sender_cfg = P2pPeerConfig::for_sender(
            session_id,
            VideoResolution { width, height },
            2_000_000,
            30.0,
        );
        sender_cfg.stun_urls.clear();
        sender_cfg.stats_interval = Duration::from_millis(100);

        let mut receiver_cfg = P2pPeerConfig::for_receiver(session_id);
        receiver_cfg.stun_urls.clear();
        receiver_cfg.stats_interval = Duration::from_millis(100);

        let (sender, mut sender_events) = P2pPeer::new_sender(sender_cfg).unwrap();
        let (receiver, mut receiver_events) = P2pPeer::new_receiver(receiver_cfg).unwrap();
        assert_eq!(sender.role(), P2pRole::Sender);
        assert_eq!(receiver.role(), P2pRole::Receiver);
        assert_eq!(sender.session_id(), session_id);
        assert!(sender.video_source().is_some());
        assert!(receiver.video_source().is_none());
        sender
            .set_bitrate(BitrateSettings {
                min_bitrate_bps: Some(250_000),
                start_bitrate_bps: Some(1_000_000),
                max_bitrate_bps: Some(2_500_000),
            })
            .unwrap();

        // Buffer a synthetic-early candidate before remote description to exercise the queue.
        let offer_signal = round_trip_signal(sender.create_offer(1).await.unwrap());
        let P2pSignal::Offer { sdp: offer_sdp, .. } = offer_signal else {
            panic!("expected offer signal");
        };
        assert!(offer_sdp.contains("H264") || offer_sdp.contains("h264"));

        // Drain any early sender ICE candidates and feed them BEFORE accept_offer to prove buffering.
        tokio::time::sleep(Duration::from_millis(50)).await;
        while let Ok(event) = sender_events.try_recv() {
            if let P2pPeerEvent::IceCandidate(signal) = event
                && let P2pSignal::Ice {
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                    ..
                } = round_trip_signal(signal)
            {
                receiver
                    .add_ice_candidate(&candidate, &sdp_mid, sdp_mline_index)
                    .await
                    .unwrap();
            }
        }

        let answer_signal = round_trip_signal(receiver.accept_offer(&offer_sdp).await.unwrap());
        let P2pSignal::Answer {
            sdp: answer_sdp, ..
        } = answer_signal
        else {
            panic!("expected answer signal");
        };
        sender.accept_answer(&answer_sdp).await.unwrap();

        let slot = FrameSlot::new();
        let mut last_seen = 0u64;
        let mut delivered = None;
        let mut last_sender_stats: Option<P2pStatsSnapshot> = None;
        let mut last_receiver_stats: Option<P2pStatsSnapshot> = None;
        let mut idr_id = 0u32;
        let started = Instant::now();

        let delivered = timeout(Duration::from_secs(12), async {
            let mut ticker = tokio::time::interval(Duration::from_millis(33));
            loop {
                tokio::select! {
                    Some(event) = sender_events.recv() => {
                        match event {
                            P2pPeerEvent::IceCandidate(signal) => {
                                if let P2pSignal::Ice { candidate, sdp_mid, sdp_mline_index, .. } = round_trip_signal(signal) {
                                    let _ = receiver.add_ice_candidate(&candidate, &sdp_mid, sdp_mline_index).await;
                                }
                            }
                            P2pPeerEvent::Stats(snapshot) => {
                                if snapshot.bytes_sent > 0 {
                                    last_sender_stats = Some(snapshot);
                                }
                            }
                            P2pPeerEvent::StateChanged(state) => {
                                assert!(!state.is_terminal(), "sender failed unexpectedly: {state:?}");
                            }
                            P2pPeerEvent::VideoTrack(_) => {}
                        }
                    }
                    Some(event) = receiver_events.recv() => {
                        match event {
                            P2pPeerEvent::IceCandidate(signal) => {
                                if let P2pSignal::Ice { candidate, sdp_mid, sdp_mline_index, .. } = round_trip_signal(signal) {
                                    let _ = sender.add_ice_candidate(&candidate, &sdp_mid, sdp_mline_index).await;
                                }
                            }
                            P2pPeerEvent::VideoTrack(track) => {
                                receiver.attach_frame_slot(track, slot.clone());
                            }
                            P2pPeerEvent::Stats(snapshot) => {
                                if snapshot.bytes_received > 0
                                    && (snapshot.frames_decoded > 0 || snapshot.frames_received > 0)
                                {
                                    last_receiver_stats = Some(snapshot);
                                }
                            }
                            P2pPeerEvent::StateChanged(state) => {
                                assert!(!state.is_terminal(), "receiver failed unexpectedly: {state:?}");
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        let _ = sender.take_keyframe_request();
                        let _ = sender.take_rate_control_request();
                        let payload = synthetic_h264_keyframe(width, height, idr_id % 16, 180, 80, 200);
                        idr_id = idr_id.wrapping_add(1);
                        let timestamp_us = i64::try_from(started.elapsed().as_micros()).unwrap_or(i64::MAX);
                        let _ = sender.push_encoded_frame(&EncodedVideoFrame {
                            codec: EncodedVideoCodec::H264,
                            payload: &payload,
                            timestamp_us,
                            frame_type: EncodedFrameType::Key,
                            resolution: VideoResolution { width, height },
                            frame_metadata: None,
                        });
                        if let Some(frame) = slot.take_if_newer(&mut last_seen) {
                            delivered = Some(frame);
                        }
                        if let Some(frame) = &delivered
                            && last_sender_stats.is_some()
                            && last_receiver_stats.is_some()
                        {
                            break Arc::clone(frame);
                        }
                    }
                }
            }
        })
        .await
        .expect("timed out waiting for decoded P2P video frame and periodic stats");

        assert!(delivered.is_valid());
        assert_eq!((delivered.width, delivered.height), (width, height));
        assert!(delivered.rgba.iter().any(|&px| px != 0));
        assert!(last_sender_stats.is_some());
        assert!(last_receiver_stats.is_some());

        let sender_stats = sender.stats().await.unwrap();
        let receiver_stats = receiver.stats().await.unwrap();
        assert!(sender_stats.bytes_sent > 0, "sender_stats={sender_stats:?}");
        assert!(
            receiver_stats.bytes_received > 0,
            "receiver_stats={receiver_stats:?}"
        );
        assert!(
            receiver_stats.frames_decoded > 0 || receiver_stats.frames_received > 0,
            "receiver_stats={receiver_stats:?}"
        );

        sender.close();
        receiver.close();
        assert_eq!(sender.state(), P2pPeerState::Closed);
        assert_eq!(receiver.state(), P2pPeerState::Closed);
    }
}
