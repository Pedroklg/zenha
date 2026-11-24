//! Coordenador P2P: executa as ações de [`super::state`] com peers `libwebrtc`, a sala e o `Watch`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use livekit::prelude::Room;
use livekit::webrtc::video_source::VideoResolution;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use zenha_protocol::P2pSignal;

use crate::backend::BackendEvent;
use crate::livekit_backend::video::{RawVideoFanOut, Watch};
use crate::livekit_backend::{Flags, TaskGuard, data, events::IncomingP2pSignal, room};
use crate::video::SharePreset;
use crate::video::gpu_share::VideoFanOut;

use super::health::{CsvRow, HealthMonitor, StatsCsv};
use super::state::{Action, P2pState};
use super::{
    P2pConfig, P2pFallbackReason, P2pPeer, P2pPeerConfig, P2pPeerEvent, P2pPeerState, P2pRole,
    P2pStatsSnapshot,
};

const TICK: Duration = Duration::from_millis(250);
const SIGNAL_SEND_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub(crate) enum P2pVideoSource {
    /// H.264 from the GPU encoder, shared with the SFU track.
    Encoded(Arc<VideoFanOut>),
    /// Raw test-pattern frames that `libwebrtc` encodes per peer (e2e only).
    Raw(Arc<RawVideoFanOut>),
}

#[derive(Clone)]
pub(crate) struct P2pShareSource {
    pub(crate) video: P2pVideoSource,
    pub(crate) preset: SharePreset,
    pub(crate) resolution: (u32, u32),
}

impl P2pShareSource {
    fn remove_sink(&self, sink_id: &str) {
        match &self.video {
            P2pVideoSource::Encoded(fan_out) => {
                fan_out.remove(sink_id);
            }
            P2pVideoSource::Raw(fan_out) => fan_out.remove(sink_id),
        }
    }
}

enum Cmd {
    ShareStarted {
        epoch: u64,
        source: Option<P2pShareSource>,
    },
    ShareStopped,
    EpochChanged(u64),
    WatchStarted {
        broadcaster: String,
        epoch: u64,
    },
    WatchStopped,
    ParticipantLeft(String),
    RemoteShareStopped(String),
}

/// Fire-and-forget commands; dropping the coordinator task closes every peer.
#[derive(Clone)]
pub(crate) struct P2pCoordinatorHandle {
    tx: UnboundedSender<Cmd>,
}

impl P2pCoordinatorHandle {
    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// `source` is `None` on the CPU capture path, which cannot feed P2P.
    pub(crate) fn share_started(&self, epoch: u64, source: Option<P2pShareSource>) {
        self.send(Cmd::ShareStarted { epoch, source });
    }

    pub(crate) fn share_stopped(&self) {
        self.send(Cmd::ShareStopped);
    }

    pub(crate) fn epoch_changed(&self, epoch: u64) {
        self.send(Cmd::EpochChanged(epoch));
    }

    pub(crate) fn watch_started(&self, broadcaster: String, epoch: u64) {
        self.send(Cmd::WatchStarted { broadcaster, epoch });
    }

    pub(crate) fn watch_stopped(&self) {
        self.send(Cmd::WatchStopped);
    }

    pub(crate) fn participant_left(&self, identity: String) {
        self.send(Cmd::ParticipantLeft(identity));
    }

    pub(crate) fn remote_share_stopped(&self, identity: String) {
        self.send(Cmd::RemoteShareStopped(identity));
    }
}

pub(crate) fn spawn_p2p_coordinator(
    room: Arc<Room>,
    events_tx: UnboundedSender<BackendEvent>,
    flags: Arc<Flags>,
    watch: Arc<Watch>,
    config: P2pConfig,
) -> (P2pCoordinatorHandle, JoinHandle<()>) {
    let signals = flags.subscribe_p2p_signals();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (peer_tx, peer_rx) = mpsc::unbounded_channel();
    let csv = config.stats_log.as_deref().and_then(|path| {
        StatsCsv::open(path)
            .inspect(|_| tracing::info!(path = %path.display(), "p2p stats csv enabled"))
            .inspect_err(
                |err| tracing::warn!(path = %path.display(), %err, "opening the p2p stats csv failed"),
            )
            .ok()
    });
    tracing::info!(
        enabled = config.enabled,
        max_viewers = config.max_viewers,
        "p2p share configured"
    );
    let coordinator = Coordinator {
        room,
        events_tx,
        flags,
        watch,
        state: P2pState::new(config.enabled, config.max_viewers),
        source: None,
        senders: HashMap::new(),
        receiver: None,
        peer_tx,
        csv,
    };
    let task = tokio::spawn(coordinator.run(cmd_rx, signals, peer_rx));
    (P2pCoordinatorHandle { tx: cmd_tx }, task)
}

struct PeerEvent {
    session_id: String,
    event: P2pPeerEvent,
}

struct Link {
    session_id: String,
    peer: P2pPeer,
    route: Option<String>,
    _forwarder: TaskGuard,
}

struct Sender {
    link: Link,
    health: Option<HealthMonitor>,
}

struct Receiver {
    broadcaster: String,
    link: Link,
}

struct Coordinator {
    room: Arc<Room>,
    events_tx: UnboundedSender<BackendEvent>,
    flags: Arc<Flags>,
    watch: Arc<Watch>,
    state: P2pState,
    source: Option<P2pShareSource>,
    senders: HashMap<String, Sender>,
    receiver: Option<Receiver>,
    peer_tx: UnboundedSender<PeerEvent>,
    csv: Option<StatsCsv>,
}

impl Coordinator {
    async fn run(
        mut self,
        mut cmds: UnboundedReceiver<Cmd>,
        mut signals: UnboundedReceiver<IncomingP2pSignal>,
        mut peer_events: UnboundedReceiver<PeerEvent>,
    ) {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let actions = tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(cmd) => self.command(cmd),
                    None => return,
                },
                Some(incoming) = signals.recv() => {
                    self.state.signal(&incoming.sender, incoming.signal, Instant::now())
                }
                Some(event) = peer_events.recv() => self.peer_event(event),
                _ = ticker.tick() => {
                    let now = Instant::now();
                    let status = self
                        .receiver
                        .as_ref()
                        .and_then(|receiver| self.watch.p2p_status(&receiver.broadcaster));
                    self.state.tick(
                        now,
                        status.map(|(mode, _)| mode),
                        status.and_then(|(_, last_frame)| last_frame),
                    )
                }
            };
            self.execute(actions).await;
        }
    }

    fn command(&mut self, cmd: Cmd) -> Vec<Action> {
        match cmd {
            Cmd::ShareStarted { epoch, source } => {
                let actions = self.state.share_started(epoch, source.is_some());
                self.close_all_senders(P2pFallbackReason::ShareStopped);
                self.source = source;
                actions
            }
            Cmd::ShareStopped => self.state.share_stopped(),
            Cmd::EpochChanged(epoch) => self.state.epoch_changed(epoch),
            Cmd::WatchStarted { broadcaster, epoch } => self.state.watch_started(
                &broadcaster,
                epoch,
                uuid::Uuid::new_v4().to_string(),
                Instant::now(),
            ),
            Cmd::WatchStopped => self.state.watch_stopped(),
            Cmd::ParticipantLeft(identity) => self.state.participant_left(&identity),
            Cmd::RemoteShareStopped(identity) => self.state.remote_share_stopped(&identity),
        }
    }

    fn peer_event(&mut self, PeerEvent { session_id, event }: PeerEvent) -> Vec<Action> {
        let now = Instant::now();
        if let Some(receiver) = self
            .receiver
            .as_mut()
            .filter(|receiver| receiver.link.session_id == session_id)
        {
            return match event {
                P2pPeerEvent::IceCandidate(signal) => vec![Action::Send {
                    to: receiver.broadcaster.clone(),
                    signal,
                }],
                P2pPeerEvent::StateChanged(state) => {
                    self.state.receiver_state(&session_id, state, now)
                }
                P2pPeerEvent::VideoTrack(track) => {
                    self.watch.start_p2p_stream(
                        &receiver.broadcaster,
                        track,
                        self.events_tx.clone(),
                        Arc::clone(&self.flags.ui_visible.0),
                    );
                    Vec::new()
                }
                P2pPeerEvent::Stats(stats) => {
                    receiver.link.route.clone_from(&stats.route);
                    record(
                        self.csv.as_ref(),
                        P2pRole::Receiver,
                        &receiver.broadcaster,
                        &session_id,
                        ("stats", ""),
                        1,
                        &stats,
                    );
                    Vec::new()
                }
            };
        }
        let Some((viewer, sender)) = self
            .senders
            .iter_mut()
            .find(|(_, sender)| sender.link.session_id == session_id)
        else {
            return Vec::new();
        };
        let viewer = viewer.clone();
        match event {
            P2pPeerEvent::IceCandidate(signal) => vec![Action::Send { to: viewer, signal }],
            P2pPeerEvent::StateChanged(P2pPeerState::Connected) => {
                if sender.health.is_none() {
                    let preset_bps = self
                        .source
                        .as_ref()
                        .map_or(0, |source| source.preset.max_bitrate_bps());
                    sender.health = Some(HealthMonitor::new(preset_bps, now));
                    log_outcome(P2pRole::Sender, &viewer, &session_id, None, None);
                }
                Vec::new()
            }
            P2pPeerEvent::StateChanged(P2pPeerState::Failed) => {
                self.state.sender_failed(&viewer, &session_id)
            }
            P2pPeerEvent::StateChanged(_) | P2pPeerEvent::VideoTrack(_) => Vec::new(),
            P2pPeerEvent::Stats(stats) => {
                sender.link.route.clone_from(&stats.route);
                let degraded = sender
                    .health
                    .as_mut()
                    .and_then(|health| health.evaluate(&stats, now));
                let viewers = self.senders.len();
                record(
                    self.csv.as_ref(),
                    P2pRole::Sender,
                    &viewer,
                    &session_id,
                    ("stats", ""),
                    viewers,
                    &stats,
                );
                let Some(cause) = degraded else {
                    return Vec::new();
                };
                tracing::warn!(
                    peer = %viewer,
                    %session_id,
                    cause = cause.as_str(),
                    available_outgoing_bitrate_bps = ?stats.available_outgoing_bitrate_bps,
                    target_bitrate_bps = ?stats.target_bitrate_bps,
                    fraction_lost = ?stats.fraction_lost,
                    "p2p upload degraded, moving viewers back to the sfu"
                );
                record(
                    self.csv.as_ref(),
                    P2pRole::Sender,
                    &viewer,
                    &session_id,
                    ("fallback", cause.as_str()),
                    viewers,
                    &stats,
                );
                self.state.bandwidth_degraded(now)
            }
        }
    }

    async fn execute(&mut self, actions: Vec<Action>) {
        let mut queue = VecDeque::from(actions);
        while let Some(action) = queue.pop_front() {
            queue.extend(self.apply(action).await);
        }
    }

    /// Returns the follow-up actions when a step fails.
    async fn apply(&mut self, action: Action) -> Vec<Action> {
        match action {
            Action::Send { to, signal } => {
                self.send(&to, &signal).await;
                Vec::new()
            }
            Action::OpenSender {
                viewer,
                session_id,
                epoch,
            } => {
                if self.open_sender(&viewer, &session_id, epoch).await {
                    Vec::new()
                } else {
                    self.state.sender_failed(&viewer, &session_id)
                }
            }
            Action::AcceptAnswer { viewer, sdp } => {
                let Some(sender) = self.senders.get(&viewer) else {
                    return Vec::new();
                };
                match sender.link.peer.accept_answer(&sdp).await {
                    Ok(()) => Vec::new(),
                    Err(err) => {
                        tracing::warn!(peer = %viewer, %err, "accepting the p2p answer failed");
                        let session_id = sender.link.session_id.clone();
                        self.state.sender_failed(&viewer, &session_id)
                    }
                }
            }
            Action::SenderIce { viewer, candidate } => {
                if let Some(sender) = self.senders.get(&viewer) {
                    add_ice(&sender.link, &candidate).await;
                }
                Vec::new()
            }
            Action::CloseSender { viewer, reason } => {
                self.close_sender(&viewer, reason);
                Vec::new()
            }
            Action::OpenReceiver {
                broadcaster,
                session_id,
                sdp,
            } => {
                if self.open_receiver(broadcaster, &session_id, &sdp).await {
                    Vec::new()
                } else {
                    self.state.receiver_failed(&session_id)
                }
            }
            Action::ReceiverIce { candidate } => {
                if let Some(receiver) = &self.receiver {
                    add_ice(&receiver.link, &candidate).await;
                }
                Vec::new()
            }
            Action::UnsubscribeSfu { broadcaster } => {
                room::set_screen_share_video_subscribed(&self.room, &broadcaster, false);
                if let Some(receiver) = &self.receiver {
                    log_outcome(
                        P2pRole::Receiver,
                        &broadcaster,
                        &receiver.link.session_id,
                        receiver.link.route.as_deref(),
                        None,
                    );
                }
                Vec::new()
            }
            Action::BeginDowngrade {
                broadcaster,
                reason,
            } => {
                if let Some(receiver) = &self.receiver {
                    log_outcome(
                        P2pRole::Receiver,
                        &broadcaster,
                        &receiver.link.session_id,
                        receiver.link.route.as_deref(),
                        Some(reason.as_str()),
                    );
                }
                if self.watch.begin_downgrade(&broadcaster) {
                    self.resubscribe_sfu(&broadcaster);
                }
                Vec::new()
            }
            Action::CloseReceiver {
                broadcaster,
                reason,
            } => {
                let receiver = self.receiver.take();
                if let Some(reason) = reason {
                    log_outcome(
                        P2pRole::Receiver,
                        &broadcaster,
                        receiver
                            .as_ref()
                            .map_or("", |receiver| receiver.link.session_id.as_str()),
                        receiver
                            .as_ref()
                            .and_then(|receiver| receiver.link.route.as_deref()),
                        Some(&reason),
                    );
                }
                drop(receiver);
                if self.watch.end_p2p(&broadcaster) {
                    self.resubscribe_sfu(&broadcaster);
                }
                Vec::new()
            }
        }
    }

    async fn open_sender(&mut self, viewer: &str, session_id: &str, epoch: u64) -> bool {
        let Some(source) = self.source.clone() else {
            return false;
        };
        let (width, height) = source.resolution;
        let config = P2pPeerConfig::for_sender(
            session_id,
            VideoResolution { width, height },
            source.preset.max_bitrate_bps(),
            f64::from(source.preset.fps()),
        );
        let pre_encoded = matches!(source.video, P2pVideoSource::Encoded(_));
        let (peer, events) = match P2pPeer::new_sender(config, pre_encoded) {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(peer = viewer, %err, "creating the p2p sender failed");
                return false;
            }
        };
        let sink = sink_id(session_id);
        if let Some(video) = peer.video_source().cloned() {
            match &source.video {
                P2pVideoSource::Encoded(fan_out) => fan_out.add(sink.clone(), video),
                P2pVideoSource::Raw(fan_out) => fan_out.add(sink.clone(), video),
            }
        }
        let offer = match peer.create_offer(epoch).await {
            Ok(offer) => offer,
            Err(err) => {
                tracing::warn!(peer = viewer, %err, "creating the p2p offer failed");
                source.remove_sink(&sink);
                return false;
            }
        };
        let link = self.link(session_id, peer, events);
        if let Some(old) = self
            .senders
            .insert(viewer.to_owned(), Sender { link, health: None })
        {
            source.remove_sink(&sink_id(&old.link.session_id));
        }
        tracing::info!(peer = viewer, %session_id, "p2p offer sent");
        self.send(viewer, &offer).await
    }

    async fn open_receiver(&mut self, broadcaster: String, session_id: &str, sdp: &str) -> bool {
        self.receiver = None;
        let (peer, events) = match P2pPeer::new_receiver(P2pPeerConfig::for_receiver(session_id)) {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(peer = %broadcaster, %err, "creating the p2p receiver failed");
                return false;
            }
        };
        let answer = match peer.accept_offer(sdp).await {
            Ok(answer) => answer,
            Err(err) => {
                tracing::warn!(peer = %broadcaster, %err, "accepting the p2p offer failed");
                return false;
            }
        };
        let link = self.link(session_id, peer, events);
        let sent = self.send(&broadcaster, &answer).await;
        self.receiver = Some(Receiver { broadcaster, link });
        sent
    }

    fn link(
        &self,
        session_id: &str,
        peer: P2pPeer,
        mut events: UnboundedReceiver<P2pPeerEvent>,
    ) -> Link {
        let tx = self.peer_tx.clone();
        let id = session_id.to_owned();
        let forwarder = TaskGuard(tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let event = PeerEvent {
                    session_id: id.clone(),
                    event,
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        }));
        Link {
            session_id: session_id.to_owned(),
            peer,
            route: None,
            _forwarder: forwarder,
        }
    }

    fn close_sender(&mut self, viewer: &str, reason: P2pFallbackReason) {
        let Some(sender) = self.senders.remove(viewer) else {
            return;
        };
        if let Some(source) = &self.source {
            source.remove_sink(&sink_id(&sender.link.session_id));
        }
        let remaining = self.senders.len();
        tracing::info!(
            role = "sender",
            peer = viewer,
            session_id = %sender.link.session_id,
            reason = reason.as_str(),
            route = sender.link.route.as_deref().unwrap_or("none"),
            active_p2p_viewers = remaining,
            "p2p sender closed"
        );
        record(
            self.csv.as_ref(),
            P2pRole::Sender,
            viewer,
            &sender.link.session_id,
            ("closed", reason.as_str()),
            remaining,
            &P2pStatsSnapshot {
                route: sender.link.route.clone(),
                ..Default::default()
            },
        );
    }

    fn close_all_senders(&mut self, reason: P2pFallbackReason) {
        let viewers: Vec<String> = self.senders.keys().cloned().collect();
        for viewer in viewers {
            self.close_sender(&viewer, reason);
        }
    }

    fn resubscribe_sfu(&self, broadcaster: &str) {
        let ui_visible = &self.flags.ui_visible.0;
        if !ui_visible.load(Ordering::SeqCst) {
            return;
        }
        if let Some(track) = room::set_screen_share_video_subscribed(&self.room, broadcaster, true)
        {
            self.watch.start_stream(
                broadcaster,
                track,
                self.events_tx.clone(),
                Arc::clone(ui_visible),
            );
        }
    }

    async fn send(&self, to: &str, signal: &P2pSignal) -> bool {
        match timeout(
            SIGNAL_SEND_TIMEOUT,
            data::send_p2p_signal(&self.room, to, signal),
        )
        .await
        {
            Ok(Ok(())) => true,
            Ok(Err(err)) => {
                tracing::debug!(to, %err, "sending a p2p signal failed");
                false
            }
            Err(_) => {
                tracing::warn!(to, "sending a p2p signal timed out");
                false
            }
        }
    }
}

fn sink_id(session_id: &str) -> String {
    format!("p2p:{session_id}")
}

async fn add_ice(link: &Link, candidate: &super::IceCandidateData) {
    if let Err(err) = link
        .peer
        .add_ice_candidate(
            &candidate.candidate,
            &candidate.sdp_mid,
            candidate.sdp_mline_index,
        )
        .await
    {
        tracing::debug!(session_id = %link.session_id, %err, "adding a p2p ice candidate failed");
    }
}

/// One line per outcome, for `zenha-e2e` and for reading the logs of a real call.
fn log_outcome(
    role: P2pRole,
    peer: &str,
    session_id: &str,
    route: Option<&str>,
    fallback: Option<&str>,
) {
    tracing::info!(
        role = role.as_str(),
        peer,
        session_id,
        outcome = if fallback.is_some() {
            "sfu_fallback"
        } else {
            "p2p_active"
        },
        reason = fallback.unwrap_or("none"),
        route = route.unwrap_or("none"),
        "p2p outcome"
    );
}

fn record(
    csv: Option<&StatsCsv>,
    role: P2pRole,
    peer: &str,
    session_id: &str,
    (event, reason): (&str, &str),
    active_p2p_viewers: usize,
    stats: &P2pStatsSnapshot,
) {
    if let Some(csv) = csv {
        csv.record(
            &CsvRow {
                role,
                peer,
                session_id,
                event,
                reason,
                active_p2p_viewers,
            },
            stats,
        );
    }
}
