//! Conexão WebRTC ponto a ponto (P2P) sobre `libwebrtc`, usada para transmissão direta de tela.

mod peer;

use std::time::Duration;

use livekit::webrtc::peer_connection::{BitrateSettings, PeerConnectionState};
use livekit::webrtc::peer_connection_factory::{
    ContinualGatheringPolicy, IceServer, IceTransportsType, RtcConfiguration,
};
use livekit::webrtc::rtp_parameters::RtpCodecCapability;
use livekit::webrtc::stats::{IceCandidatePairState, RtcStats};
use livekit::webrtc::video_source::VideoResolution;
use livekit::webrtc::video_track::RtcVideoTrack;
use zenha_protocol::P2pSignal;

#[expect(
    unused_imports,
    reason = "re-exported for the P2P coordinator in the next phase"
)]
pub(crate) use self::peer::{P2pPeer, P2pPeerError};

pub(crate) const DEFAULT_STUN_URLS: [&str; 2] = [
    "stun:stun.l.google.com:19302",
    "stun:stun.cloudflare.com:3478",
];
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) const MAX_BUFFERED_ICE_CANDIDATES: usize = 64;
pub(crate) const DEFAULT_STATS_INTERVAL: Duration = Duration::from_secs(2);
const H264_MIME_TYPE: &str = "video/h264";
const H264_CONSTRAINED_BASELINE_FMTP: &str = "profile-level-id=42e01f";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) enum P2pRole {
    Sender,
    Receiver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) enum P2pPeerState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl P2pPeerState {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
    )]
    pub(crate) fn from_rtc(state: PeerConnectionState) -> Self {
        match state {
            PeerConnectionState::New => Self::New,
            PeerConnectionState::Connecting => Self::Connecting,
            PeerConnectionState::Connected => Self::Connected,
            PeerConnectionState::Disconnected => Self::Disconnected,
            PeerConnectionState::Failed => Self::Failed,
            PeerConnectionState::Closed => Self::Closed,
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
    )]
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Closed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) struct IceCandidateData {
    pub(crate) candidate: String,
    pub(crate) sdp_mid: String,
    pub(crate) sdp_mline_index: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) enum IceBufferAction {
    ApplyNow(IceCandidateData),
    Buffered,
    LimitReached,
}

/// Buffers remote ICE candidates received before `set_remote_description` completes.
#[derive(Debug)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) struct IceCandidateBuffer {
    remote_description_set: bool,
    pending: Vec<IceCandidateData>,
    received_count: usize,
    max_candidates: usize,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
impl IceCandidateBuffer {
    pub(crate) fn new(max_candidates: usize) -> Self {
        Self {
            remote_description_set: false,
            pending: Vec::new(),
            received_count: 0,
            max_candidates,
        }
    }

    pub(crate) fn push(&mut self, candidate: IceCandidateData) -> IceBufferAction {
        if self.received_count >= self.max_candidates {
            return IceBufferAction::LimitReached;
        }
        self.received_count += 1;
        if self.remote_description_set {
            IceBufferAction::ApplyNow(candidate)
        } else {
            self.pending.push(candidate);
            IceBufferAction::Buffered
        }
    }

    pub(crate) fn mark_remote_description_set(&mut self) -> Vec<IceCandidateData> {
        self.remote_description_set = true;
        std::mem::take(&mut self.pending)
    }

    pub(crate) fn buffered_len(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn received_count(&self) -> usize {
        self.received_count
    }
}

impl Default for IceCandidateBuffer {
    fn default() -> Self {
        Self::new(MAX_BUFFERED_ICE_CANDIDATES)
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) struct P2pPeerConfig {
    pub(crate) session_id: String,
    pub(crate) stun_urls: Vec<String>,
    pub(crate) video_resolution: VideoResolution,
    pub(crate) bitrate: BitrateSettings,
    pub(crate) max_framerate: Option<f64>,
    pub(crate) stats_interval: Duration,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
impl P2pPeerConfig {
    pub(crate) fn for_sender(
        session_id: impl Into<String>,
        video_resolution: VideoResolution,
        max_bitrate_bps: u64,
        max_framerate: f64,
    ) -> Self {
        let min_bitrate_bps = 300_000u64.min(max_bitrate_bps);
        let start_bitrate_bps = (max_bitrate_bps / 2).clamp(min_bitrate_bps, max_bitrate_bps);
        Self {
            session_id: session_id.into(),
            stun_urls: DEFAULT_STUN_URLS
                .iter()
                .map(|url| (*url).to_owned())
                .collect(),
            video_resolution,
            bitrate: BitrateSettings {
                min_bitrate_bps: Some(min_bitrate_bps),
                start_bitrate_bps: Some(start_bitrate_bps),
                max_bitrate_bps: Some(max_bitrate_bps),
            },
            max_framerate: Some(max_framerate),
            stats_interval: DEFAULT_STATS_INTERVAL,
        }
    }

    pub(crate) fn for_receiver(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            stun_urls: DEFAULT_STUN_URLS
                .iter()
                .map(|url| (*url).to_owned())
                .collect(),
            video_resolution: VideoResolution::default(),
            bitrate: BitrateSettings::default(),
            max_framerate: None,
            stats_interval: DEFAULT_STATS_INTERVAL,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) struct P2pStatsSnapshot {
    pub(crate) timestamp_us: i64,
    pub(crate) bytes_sent: u64,
    pub(crate) bytes_received: u64,
    pub(crate) bitrate_bps: Option<f64>,
    pub(crate) rtt_ms: Option<f64>,
    pub(crate) packets_sent: u64,
    pub(crate) packets_received: u64,
    pub(crate) packets_lost: i64,
    pub(crate) frames_sent: u32,
    pub(crate) frames_encoded: u32,
    pub(crate) frames_received: u64,
    pub(crate) frames_decoded: u32,
    pub(crate) frames_dropped: u32,
    pub(crate) frame_width: u32,
    pub(crate) frame_height: u32,
    pub(crate) frames_per_second: f64,
}

#[derive(Debug)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) enum P2pPeerEvent {
    IceCandidate(P2pSignal),
    StateChanged(P2pPeerState),
    VideoTrack(RtcVideoTrack),
    Stats(P2pStatsSnapshot),
}

/// Builds the `RtcConfiguration` for P2P connections (public STUN only, all ICE transports).
///
/// Note: `libwebrtc` 0.4.2 does not expose `bundle_policy` on `RtcConfiguration`;
/// single-track video P2P only negotiates one media section.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) fn build_rtc_configuration(stun_urls: &[String]) -> RtcConfiguration {
    let mut config = RtcConfiguration::default();
    if !stun_urls.is_empty() {
        config.ice_servers = vec![IceServer {
            urls: stun_urls.to_vec(),
            username: String::new(),
            password: String::new(),
        }];
    }
    config.ice_transport_type = IceTransportsType::All;
    config.continual_gathering_policy = ContinualGatheringPolicy::GatherContinually;
    config
}

/// Returns video codec capabilities prioritizing H.264 (Constrained Baseline 42e01f first).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) fn prioritize_h264_codecs(
    capabilities: Vec<RtpCodecCapability>,
) -> Vec<RtpCodecCapability> {
    let mut baseline_h264 = Vec::new();
    let mut other_h264 = Vec::new();
    let mut fallback = Vec::new();

    for codec in capabilities {
        if codec.mime_type.eq_ignore_ascii_case(H264_MIME_TYPE) {
            let is_constrained_baseline = codec.sdp_fmtp_line.as_deref().is_some_and(|line| {
                line.to_ascii_lowercase()
                    .contains(H264_CONSTRAINED_BASELINE_FMTP)
            });
            if is_constrained_baseline {
                baseline_h264.push(codec);
            } else {
                other_h264.push(codec);
            }
        } else {
            fallback.push(codec);
        }
    }

    baseline_h264.append(&mut other_h264);
    if baseline_h264.is_empty() {
        fallback
    } else {
        baseline_h264
    }
}

/// Extracts media and transport counters from a `PeerConnection::get_stats()` report.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the P2P coordinator in the next phase")
)]
pub(crate) fn extract_stats_snapshot(
    stats: &[RtcStats],
    previous: Option<&P2pStatsSnapshot>,
) -> P2pStatsSnapshot {
    let mut snapshot = P2pStatsSnapshot::default();
    let mut fallback_target_bitrate: Option<f64> = None;
    let mut remote_rtt_ms: Option<f64> = None;

    for stat in stats {
        match stat {
            RtcStats::OutboundRtp(outbound)
                if outbound.stream.kind.eq_ignore_ascii_case("video") =>
            {
                snapshot.timestamp_us = snapshot.timestamp_us.max(outbound.rtc.timestamp);
                snapshot.bytes_sent = snapshot.bytes_sent.saturating_add(outbound.sent.bytes_sent);
                snapshot.packets_sent = snapshot
                    .packets_sent
                    .saturating_add(outbound.sent.packets_sent);
                snapshot.frames_sent = snapshot
                    .frames_sent
                    .saturating_add(outbound.outbound.frames_sent);
                snapshot.frames_encoded = snapshot
                    .frames_encoded
                    .saturating_add(outbound.outbound.frames_encoded);
                if outbound.outbound.frame_width > 0 {
                    snapshot.frame_width = outbound.outbound.frame_width;
                    snapshot.frame_height = outbound.outbound.frame_height;
                }
                if outbound.outbound.frames_per_second > 0.0 {
                    snapshot.frames_per_second = outbound.outbound.frames_per_second;
                }
                if outbound.outbound.target_bitrate > 0.0 {
                    fallback_target_bitrate = Some(outbound.outbound.target_bitrate);
                }
            }
            RtcStats::InboundRtp(inbound) if inbound.stream.kind.eq_ignore_ascii_case("video") => {
                snapshot.timestamp_us = snapshot.timestamp_us.max(inbound.rtc.timestamp);
                snapshot.bytes_received = snapshot
                    .bytes_received
                    .saturating_add(inbound.inbound.bytes_received);
                snapshot.packets_received = snapshot
                    .packets_received
                    .saturating_add(inbound.received.packets_received);
                snapshot.packets_lost = snapshot
                    .packets_lost
                    .saturating_add(inbound.received.packets_lost);
                snapshot.frames_received = snapshot
                    .frames_received
                    .saturating_add(inbound.inbound.frames_received);
                snapshot.frames_decoded = snapshot
                    .frames_decoded
                    .saturating_add(inbound.inbound.frames_decoded);
                snapshot.frames_dropped = snapshot
                    .frames_dropped
                    .saturating_add(inbound.inbound.frames_dropped);
                if inbound.inbound.frame_width > 0 {
                    snapshot.frame_width = inbound.inbound.frame_width;
                    snapshot.frame_height = inbound.inbound.frame_height;
                }
                if inbound.inbound.frames_per_second > 0.0 {
                    snapshot.frames_per_second = inbound.inbound.frames_per_second;
                }
            }
            RtcStats::RemoteInboundRtp(remote)
                if remote.stream.kind.eq_ignore_ascii_case("video") =>
            {
                snapshot.timestamp_us = snapshot.timestamp_us.max(remote.rtc.timestamp);
                snapshot.packets_lost = snapshot
                    .packets_lost
                    .saturating_add(remote.received.packets_lost);
                if remote.remote_inbound.round_trip_time > 0.0 {
                    remote_rtt_ms = Some(remote.remote_inbound.round_trip_time * 1000.0);
                }
            }
            RtcStats::CandidatePair(pair) => {
                let active = pair.candidate_pair.nominated
                    || pair.candidate_pair.state == Some(IceCandidatePairState::Succeeded);
                if active {
                    snapshot.timestamp_us = snapshot.timestamp_us.max(pair.rtc.timestamp);
                    if pair.candidate_pair.current_round_trip_time > 0.0 {
                        snapshot.rtt_ms =
                            Some(pair.candidate_pair.current_round_trip_time * 1000.0);
                    }
                    if fallback_target_bitrate.is_none()
                        && pair.candidate_pair.available_outgoing_bitrate > 0.0
                    {
                        fallback_target_bitrate =
                            Some(pair.candidate_pair.available_outgoing_bitrate);
                    }
                }
            }
            _ => {}
        }
    }

    if snapshot.rtt_ms.is_none() {
        snapshot.rtt_ms = remote_rtt_ms;
    }

    if let Some(prev) = previous
        && snapshot.timestamp_us > prev.timestamp_us
    {
        let elapsed_secs = (snapshot.timestamp_us - prev.timestamp_us) as f64 / 1_000_000.0;
        let delta_bytes = if snapshot.bytes_sent >= prev.bytes_sent
            && (snapshot.bytes_sent > prev.bytes_sent || snapshot.bytes_received == 0)
        {
            snapshot.bytes_sent - prev.bytes_sent
        } else {
            snapshot.bytes_received.saturating_sub(prev.bytes_received)
        };
        snapshot.bitrate_bps = Some((delta_bytes as f64) * 8.0 / elapsed_secs);
    } else {
        snapshot.bitrate_bps = fallback_target_bitrate;
    }

    snapshot
}

#[cfg(test)]
mod tests {
    use livekit::webrtc::stats::{
        CandidatePairStats, InboundRtpStats, OutboundRtpStats, RemoteInboundRtpStats, dictionaries,
    };

    use super::*;

    fn candidate(index: i32) -> IceCandidateData {
        IceCandidateData {
            candidate: format!("candidate:{index} 1 udp 1 127.0.0.1 5000 typ host"),
            sdp_mid: "0".to_owned(),
            sdp_mline_index: index,
        }
    }

    #[test]
    fn ice_candidate_buffer_queues_before_remote_description_and_flushes_in_order() {
        let mut buffer = IceCandidateBuffer::new(3);
        assert_eq!(buffer.push(candidate(0)), IceBufferAction::Buffered);
        assert_eq!(buffer.push(candidate(1)), IceBufferAction::Buffered);
        assert_eq!(buffer.buffered_len(), 2);

        let flushed = buffer.mark_remote_description_set();
        assert_eq!(flushed, vec![candidate(0), candidate(1)]);
        assert_eq!(buffer.buffered_len(), 0);

        assert_eq!(
            buffer.push(candidate(2)),
            IceBufferAction::ApplyNow(candidate(2))
        );
        assert_eq!(buffer.received_count(), 3);
        assert_eq!(buffer.push(candidate(3)), IceBufferAction::LimitReached);
    }

    #[test]
    fn rtc_configuration_uses_public_stun_and_all_ice_transports() {
        let defaults: Vec<String> = DEFAULT_STUN_URLS
            .iter()
            .map(|url| (*url).to_owned())
            .collect();
        let cfg = build_rtc_configuration(&defaults);
        assert_eq!(cfg.ice_transport_type, IceTransportsType::All);
        assert_eq!(
            cfg.continual_gathering_policy,
            ContinualGatheringPolicy::GatherContinually
        );
        assert_eq!(cfg.ice_servers.len(), 1);
        assert_eq!(cfg.ice_servers[0].urls, defaults);

        let local_only = build_rtc_configuration(&[]);
        assert!(local_only.ice_servers.is_empty());
    }

    #[test]
    fn h264_codec_prioritization_orders_constrained_baseline_first() {
        let vp8 = RtpCodecCapability {
            channels: None,
            clock_rate: Some(90_000),
            mime_type: "video/VP8".to_owned(),
            sdp_fmtp_line: None,
        };
        let h264_high = RtpCodecCapability {
            channels: None,
            clock_rate: Some(90_000),
            mime_type: "video/H264".to_owned(),
            sdp_fmtp_line: Some("profile-level-id=64001f".to_owned()),
        };
        let h264_cb = RtpCodecCapability {
            channels: None,
            clock_rate: Some(90_000),
            mime_type: "video/H264".to_owned(),
            sdp_fmtp_line: Some("level-asymmetry-allowed=1;profile-level-id=42e01f".to_owned()),
        };

        let ordered = prioritize_h264_codecs(vec![vp8.clone(), h264_high, h264_cb]);
        assert_eq!(ordered.len(), 2);
        assert_eq!(
            ordered[0].sdp_fmtp_line.as_deref(),
            Some("level-asymmetry-allowed=1;profile-level-id=42e01f")
        );
        assert_eq!(
            ordered[1].sdp_fmtp_line.as_deref(),
            Some("profile-level-id=64001f")
        );

        let fallback = prioritize_h264_codecs(vec![vp8]);
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].mime_type, "video/VP8");
    }

    #[test]
    fn peer_state_mapping_and_terminal_detection() {
        assert_eq!(
            P2pPeerState::from_rtc(PeerConnectionState::Connected),
            P2pPeerState::Connected
        );
        assert!(!P2pPeerState::Connected.is_terminal());
        assert!(P2pPeerState::Failed.is_terminal());
        assert!(P2pPeerState::Closed.is_terminal());
    }

    #[test]
    fn stats_snapshot_computes_bitrate_rtt_loss_and_frames() {
        let first = extract_stats_snapshot(
            &[
                RtcStats::OutboundRtp(OutboundRtpStats {
                    rtc: dictionaries::RtcStats {
                        id: "out".to_owned(),
                        timestamp: 1_000_000,
                    },
                    stream: dictionaries::RtpStreamStats {
                        kind: "video".to_owned(),
                        ..Default::default()
                    },
                    sent: dictionaries::SentRtpStreamStats {
                        packets_sent: 10,
                        bytes_sent: 10_000,
                    },
                    outbound: dictionaries::OutboundRtpStreamStats {
                        frames_sent: 5,
                        frames_encoded: 5,
                        frame_width: 1280,
                        frame_height: 720,
                        frames_per_second: 30.0,
                        target_bitrate: 2_000_000.0,
                        ..Default::default()
                    },
                }),
                RtcStats::RemoteInboundRtp(RemoteInboundRtpStats {
                    rtc: dictionaries::RtcStats {
                        id: "rem".to_owned(),
                        timestamp: 1_000_000,
                    },
                    stream: dictionaries::RtpStreamStats {
                        kind: "video".to_owned(),
                        ..Default::default()
                    },
                    received: dictionaries::ReceivedRtpStreamStats {
                        packets_lost: 2,
                        ..Default::default()
                    },
                    remote_inbound: dictionaries::RemoteInboundRtpStreamStats {
                        round_trip_time: 0.025,
                        ..Default::default()
                    },
                }),
            ],
            None,
        );
        assert_eq!(first.bytes_sent, 10_000);
        assert_eq!(first.packets_lost, 2);
        assert_eq!(first.frames_sent, 5);
        assert_eq!(first.rtt_ms, Some(25.0));
        assert_eq!(first.bitrate_bps, Some(2_000_000.0));

        let second = extract_stats_snapshot(
            &[
                RtcStats::InboundRtp(InboundRtpStats {
                    rtc: dictionaries::RtcStats {
                        id: "in".to_owned(),
                        timestamp: 2_000_000,
                    },
                    stream: dictionaries::RtpStreamStats {
                        kind: "video".to_owned(),
                        ..Default::default()
                    },
                    received: dictionaries::ReceivedRtpStreamStats {
                        packets_received: 50,
                        packets_lost: 1,
                        ..Default::default()
                    },
                    inbound: dictionaries::InboundRtpStreamStats {
                        bytes_received: 125_000,
                        frames_received: 30,
                        frames_decoded: 29,
                        frames_dropped: 1,
                        frame_width: 1280,
                        frame_height: 720,
                        frames_per_second: 30.0,
                        ..Default::default()
                    },
                }),
                RtcStats::CandidatePair(CandidatePairStats {
                    rtc: dictionaries::RtcStats {
                        id: "pair".to_owned(),
                        timestamp: 2_000_000,
                    },
                    candidate_pair: dictionaries::CandidatePairStats {
                        nominated: true,
                        state: Some(IceCandidatePairState::Succeeded),
                        current_round_trip_time: 0.012,
                        ..Default::default()
                    },
                }),
            ],
            Some(&P2pStatsSnapshot {
                timestamp_us: 1_000_000,
                bytes_received: 25_000,
                ..Default::default()
            }),
        );
        assert_eq!(second.bitrate_bps, Some(800_000.0));
        assert_eq!(second.rtt_ms, Some(12.0));
        assert_eq!(second.frames_decoded, 29);
        assert_eq!(second.frames_dropped, 1);
        assert_eq!(second.packets_lost, 1);
    }
}
