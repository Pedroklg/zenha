//! Máquina de estados pura do P2P: admissão no transmissor e troca SFU/P2P no espectador.
//!
//! Não faz I/O: recebe eventos com o instante atual e devolve [`Action`]s que o coordenador executa.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use zenha_protocol::P2pSignal;

use super::{IceCandidateData, P2pFallbackReason as Reason, P2pPeerState};
use crate::livekit_backend::video::WatchMode;

pub(super) const REQUEST_COOLDOWN: Duration = Duration::from_secs(5);
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const DISCONNECTED_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const FRAME_STALL_TIMEOUT: Duration = Duration::from_millis(1500);
pub(super) const DOWNGRADE_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    Send {
        to: String,
        signal: P2pSignal,
    },
    OpenSender {
        viewer: String,
        session_id: String,
        epoch: u64,
    },
    AcceptAnswer {
        viewer: String,
        sdp: String,
    },
    SenderIce {
        viewer: String,
        candidate: IceCandidateData,
    },
    CloseSender {
        viewer: String,
        reason: Reason,
    },
    OpenReceiver {
        broadcaster: String,
        session_id: String,
        sdp: String,
    },
    ReceiverIce {
        candidate: IceCandidateData,
    },
    /// The `Watch` already shows P2P frames: drop the SFU video subscription.
    UnsubscribeSfu {
        broadcaster: String,
    },
    /// Planned downgrade: subscribe to the SFU again and keep P2P until its first frame.
    BeginDowngrade {
        broadcaster: String,
        reason: Reason,
    },
    /// Ends the P2P receiver; `reason` is set when this is a fallback worth reporting.
    CloseReceiver {
        broadcaster: String,
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    None,
    Open,
    Draining { until: Instant, reason: Reason },
}

#[derive(Debug)]
struct Viewer {
    session_id: String,
    link: Link,
}

/// Local share as seen by the sharer: everyone who asked to watch counts toward the limit.
#[derive(Debug)]
struct Share {
    epoch: u64,
    p2p_capable: bool,
    viewers: BTreeMap<String, Viewer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Sfu,
    Negotiating { since: Instant, offered: bool },
    P2p { disconnected_since: Option<Instant> },
    Downgrading { until: Instant },
}

#[derive(Debug)]
struct Watching {
    broadcaster: String,
    epoch: u64,
    session_id: String,
    phase: Phase,
}

#[derive(Debug)]
pub(super) struct P2pState {
    enabled: bool,
    max_viewers: usize,
    last_request: HashMap<String, Instant>,
    share: Option<Share>,
    watching: Option<Watching>,
}

impl P2pState {
    pub(super) fn new(enabled: bool, max_viewers: usize) -> Self {
        Self {
            enabled,
            max_viewers,
            last_request: HashMap::new(),
            share: None,
            watching: None,
        }
    }

    pub(super) fn share_started(&mut self, epoch: u64, p2p_capable: bool) -> Vec<Action> {
        let actions = self.share_stopped();
        self.share = Some(Share {
            epoch,
            p2p_capable,
            viewers: BTreeMap::new(),
        });
        actions
    }

    pub(super) fn share_stopped(&mut self) -> Vec<Action> {
        let Some(share) = self.share.take() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        for (viewer, entry) in share.viewers {
            if entry.link != Link::None {
                actions.push(send(
                    &viewer,
                    P2pSignal::Bye {
                        session_id: entry.session_id,
                    },
                ));
                actions.push(Action::CloseSender {
                    viewer,
                    reason: Reason::ShareStopped,
                });
            }
        }
        actions
    }

    /// The share was republished with a new track sid: every P2P session starts over.
    pub(super) fn epoch_changed(&mut self, epoch: u64) -> Vec<Action> {
        let Some(capable) = self
            .share
            .as_ref()
            .filter(|share| share.epoch != epoch)
            .map(|share| share.p2p_capable)
        else {
            return Vec::new();
        };
        self.share_started(epoch, capable)
    }

    pub(super) fn watch_started(
        &mut self,
        broadcaster: &str,
        epoch: u64,
        session_id: String,
        now: Instant,
    ) -> Vec<Action> {
        let mut actions = self.watch_stopped();
        if !self.enabled {
            return actions;
        }
        actions.push(send(
            broadcaster,
            P2pSignal::Request {
                share_epoch: epoch,
                session_id: session_id.clone(),
            },
        ));
        self.watching = Some(Watching {
            broadcaster: broadcaster.to_owned(),
            epoch,
            session_id,
            phase: Phase::Negotiating {
                since: now,
                offered: false,
            },
        });
        actions
    }

    pub(super) fn watch_stopped(&mut self) -> Vec<Action> {
        self.end_watch(true)
    }

    pub(super) fn remote_share_stopped(&mut self, identity: &str) -> Vec<Action> {
        if self.is_watching(identity) {
            self.end_watch(true)
        } else {
            Vec::new()
        }
    }

    pub(super) fn participant_left(&mut self, identity: &str) -> Vec<Action> {
        self.last_request.remove(identity);
        let mut actions = Vec::new();
        if let Some(share) = self.share.as_mut()
            && let Some(entry) = share.viewers.remove(identity)
            && entry.link != Link::None
        {
            actions.push(Action::CloseSender {
                viewer: identity.to_owned(),
                reason: Reason::ViewerLeft,
            });
        }
        if self.is_watching(identity) {
            actions.extend(self.end_watch(false));
        }
        actions
    }

    pub(super) fn signal(&mut self, sender: &str, signal: P2pSignal, now: Instant) -> Vec<Action> {
        match signal {
            P2pSignal::Request {
                share_epoch,
                session_id,
            } => self.request(sender, share_epoch, session_id, now),
            P2pSignal::Offer {
                share_epoch,
                session_id,
                sdp,
            } => self.offer(sender, share_epoch, &session_id, sdp),
            P2pSignal::Answer { session_id, sdp } => {
                if self.viewer_link(sender, &session_id) == Some(Link::Open) {
                    vec![Action::AcceptAnswer {
                        viewer: sender.to_owned(),
                        sdp,
                    }]
                } else {
                    Vec::new()
                }
            }
            P2pSignal::Ice {
                session_id,
                candidate,
                sdp_mid,
                sdp_mline_index,
            } => {
                let candidate = IceCandidateData {
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                };
                if self
                    .viewer_link(sender, &session_id)
                    .is_some_and(|link| link != Link::None)
                {
                    vec![Action::SenderIce {
                        viewer: sender.to_owned(),
                        candidate,
                    }]
                } else if self.receiver_exists(sender, &session_id) {
                    vec![Action::ReceiverIce { candidate }]
                } else {
                    Vec::new()
                }
            }
            P2pSignal::Reject { session_id, reason } => {
                if self.watching_session(sender, &session_id)
                    && matches!(self.phase(), Some(Phase::Negotiating { .. }))
                {
                    self.viewer_fallback(reason, false)
                } else {
                    Vec::new()
                }
            }
            P2pSignal::Fallback { session_id, reason } => {
                if self
                    .viewer_link(sender, &session_id)
                    .is_some_and(|link| link != Link::None)
                {
                    return self.close_link(sender, Reason::ViewerFallback);
                }
                if !self.watching_session(sender, &session_id) {
                    return Vec::new();
                }
                match self.phase() {
                    Some(Phase::P2p { .. }) => self.begin_downgrade(&reason, now),
                    Some(Phase::Negotiating { .. }) => self.viewer_fallback(reason, false),
                    _ => Vec::new(),
                }
            }
            P2pSignal::Bye { session_id } => {
                if self.viewer_link(sender, &session_id).is_some() {
                    return self.participant_left_share(sender);
                }
                if self.watching_session(sender, &session_id) {
                    return self.viewer_fallback(Reason::ShareStopped.as_str().to_owned(), false);
                }
                Vec::new()
            }
        }
    }

    /// The sender peer could not be set up or failed: tell the viewer to stay on the SFU.
    pub(super) fn sender_failed(&mut self, viewer: &str, session_id: &str) -> Vec<Action> {
        if self
            .viewer_link(viewer, session_id)
            .is_none_or(|link| link == Link::None)
        {
            return Vec::new();
        }
        let mut actions = self.close_link(viewer, Reason::IceFailed);
        actions.push(send(
            viewer,
            P2pSignal::Fallback {
                session_id: session_id.to_owned(),
                reason: Reason::IceFailed.as_str().to_owned(),
            },
        ));
        actions
    }

    /// Upload or loss stayed bad past the hysteresis window: everyone goes back to the SFU.
    pub(super) fn bandwidth_degraded(&mut self, now: Instant) -> Vec<Action> {
        self.drain_all(Reason::BandwidthDegraded, now)
    }

    pub(super) fn receiver_failed(&mut self, session_id: &str) -> Vec<Action> {
        match &self.watching {
            Some(watching) if watching.session_id == session_id => {
                self.viewer_fallback(Reason::IceFailed.as_str().to_owned(), true)
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn receiver_state(
        &mut self,
        session_id: &str,
        state: P2pPeerState,
        now: Instant,
    ) -> Vec<Action> {
        if matches!(state, P2pPeerState::Failed | P2pPeerState::Closed) {
            return self.receiver_failed(session_id);
        }
        if let Some(watching) = self.watching.as_mut()
            && watching.session_id == session_id
            && let Phase::P2p { disconnected_since } = &mut watching.phase
        {
            match state {
                P2pPeerState::Disconnected => {
                    disconnected_since.get_or_insert(now);
                }
                P2pPeerState::Connected => *disconnected_since = None,
                _ => {}
            }
        }
        Vec::new()
    }

    /// `watch_mode` and `last_p2p_frame` come from the `Watch`, which switches transports itself.
    pub(super) fn tick(
        &mut self,
        now: Instant,
        watch_mode: Option<WatchMode>,
        last_p2p_frame: Option<Instant>,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(share) = self.share.as_mut() {
            for (viewer, entry) in &mut share.viewers {
                if let Link::Draining { until, reason } = entry.link
                    && now >= until
                {
                    entry.link = Link::None;
                    actions.push(Action::CloseSender {
                        viewer: viewer.clone(),
                        reason,
                    });
                }
            }
        }
        let Some(watching) = self.watching.as_mut() else {
            return actions;
        };
        let fallback = match watching.phase {
            Phase::Negotiating { .. } if watch_mode == Some(WatchMode::P2p) => {
                watching.phase = Phase::P2p {
                    disconnected_since: None,
                };
                actions.push(Action::UnsubscribeSfu {
                    broadcaster: watching.broadcaster.clone(),
                });
                None
            }
            Phase::Negotiating { since, .. }
                if now.saturating_duration_since(since) >= CONNECT_TIMEOUT =>
            {
                Some(Reason::NatTimeout)
            }
            Phase::P2p { disconnected_since }
                if disconnected_since.is_some_and(|since| {
                    now.saturating_duration_since(since) >= DISCONNECTED_TIMEOUT
                }) =>
            {
                Some(Reason::IceFailed)
            }
            Phase::P2p { .. }
                if last_p2p_frame
                    .is_none_or(|at| now.saturating_duration_since(at) > FRAME_STALL_TIMEOUT) =>
            {
                Some(Reason::FrameTimeout)
            }
            Phase::Downgrading { until }
                if watch_mode != Some(WatchMode::DowngradingToSfu) || now >= until =>
            {
                watching.phase = Phase::Sfu;
                actions.push(Action::CloseReceiver {
                    broadcaster: watching.broadcaster.clone(),
                    reason: None,
                });
                None
            }
            _ => None,
        };
        if let Some(reason) = fallback {
            actions.extend(self.viewer_fallback(reason.as_str().to_owned(), true));
        }
        actions
    }

    fn request(
        &mut self,
        viewer: &str,
        epoch: u64,
        session_id: String,
        now: Instant,
    ) -> Vec<Action> {
        if !self.enabled {
            return vec![reject(viewer, session_id, Reason::Disabled)];
        }
        let Some(share) = self.share.as_mut().filter(|share| share.epoch == epoch) else {
            return vec![reject(viewer, session_id, Reason::StaleEpoch)];
        };
        let mut actions = Vec::new();
        let previous = share.viewers.insert(
            viewer.to_owned(),
            Viewer {
                session_id: session_id.clone(),
                link: Link::None,
            },
        );
        if previous.is_some_and(|old| old.link != Link::None) {
            actions.push(Action::CloseSender {
                viewer: viewer.to_owned(),
                reason: Reason::ViewerLeft,
            });
        }
        let throttled = self
            .last_request
            .get(viewer)
            .is_some_and(|at| now.saturating_duration_since(*at) < REQUEST_COOLDOWN);
        if throttled {
            actions.push(reject(viewer, session_id, Reason::RateLimited));
            return actions;
        }
        self.last_request.insert(viewer.to_owned(), now);
        if !share.p2p_capable {
            actions.push(reject(viewer, session_id, Reason::CpuFallback));
            return actions;
        }
        if share.viewers.len() > self.max_viewers {
            actions.push(reject(viewer, session_id, Reason::LimitExceeded));
            actions.extend(self.drain_all(Reason::LimitExceeded, now));
            return actions;
        }
        if let Some(entry) = share.viewers.get_mut(viewer) {
            entry.link = Link::Open;
        }
        actions.push(Action::OpenSender {
            viewer: viewer.to_owned(),
            session_id,
            epoch,
        });
        actions
    }

    fn offer(&mut self, sender: &str, epoch: u64, session_id: &str, sdp: String) -> Vec<Action> {
        let Some(watching) = self.watching.as_mut().filter(|watching| {
            watching.broadcaster == sender
                && watching.epoch == epoch
                && watching.session_id == session_id
        }) else {
            tracing::warn!(%sender, %session_id, "dropping an unsolicited p2p offer");
            return Vec::new();
        };
        let Phase::Negotiating { offered, .. } = &mut watching.phase else {
            return Vec::new();
        };
        if *offered {
            return Vec::new();
        }
        *offered = true;
        vec![Action::OpenReceiver {
            broadcaster: sender.to_owned(),
            session_id: session_id.to_owned(),
            sdp,
        }]
    }

    fn drain_all(&mut self, reason: Reason, now: Instant) -> Vec<Action> {
        let Some(share) = self.share.as_mut() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        for (viewer, entry) in &mut share.viewers {
            if entry.link == Link::Open {
                entry.link = Link::Draining {
                    until: now + DOWNGRADE_GRACE,
                    reason,
                };
                actions.push(send(
                    viewer,
                    P2pSignal::Fallback {
                        session_id: entry.session_id.clone(),
                        reason: reason.as_str().to_owned(),
                    },
                ));
            }
        }
        actions
    }

    fn close_link(&mut self, viewer: &str, reason: Reason) -> Vec<Action> {
        if let Some(entry) = self
            .share
            .as_mut()
            .and_then(|share| share.viewers.get_mut(viewer))
        {
            entry.link = Link::None;
        }
        vec![Action::CloseSender {
            viewer: viewer.to_owned(),
            reason,
        }]
    }

    /// The viewer stopped watching: it no longer counts toward the limit.
    fn participant_left_share(&mut self, viewer: &str) -> Vec<Action> {
        let entry = self
            .share
            .as_mut()
            .and_then(|share| share.viewers.remove(viewer));
        match entry {
            Some(entry) if entry.link != Link::None => vec![Action::CloseSender {
                viewer: viewer.to_owned(),
                reason: Reason::ViewerLeft,
            }],
            _ => Vec::new(),
        }
    }

    fn begin_downgrade(&mut self, reason: &str, now: Instant) -> Vec<Action> {
        let Some(watching) = self.watching.as_mut() else {
            return Vec::new();
        };
        watching.phase = Phase::Downgrading {
            until: now + DOWNGRADE_GRACE,
        };
        vec![Action::BeginDowngrade {
            broadcaster: watching.broadcaster.clone(),
            reason: parse_reason(reason),
        }]
    }

    /// Gives up on P2P for this watch; `notify` asks the sharer to close its side too.
    fn viewer_fallback(&mut self, reason: String, notify: bool) -> Vec<Action> {
        let Some(watching) = self.watching.as_mut() else {
            return Vec::new();
        };
        if watching.phase == Phase::Sfu {
            return Vec::new();
        }
        watching.phase = Phase::Sfu;
        let mut actions = Vec::new();
        if notify {
            actions.push(send(
                &watching.broadcaster,
                P2pSignal::Fallback {
                    session_id: watching.session_id.clone(),
                    reason: reason.clone(),
                },
            ));
        }
        actions.push(Action::CloseReceiver {
            broadcaster: watching.broadcaster.clone(),
            reason: Some(reason),
        });
        actions
    }

    fn end_watch(&mut self, say_bye: bool) -> Vec<Action> {
        let Some(watching) = self.watching.take() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        if say_bye {
            actions.push(send(
                &watching.broadcaster,
                P2pSignal::Bye {
                    session_id: watching.session_id,
                },
            ));
        }
        if watching.phase != Phase::Sfu {
            actions.push(Action::CloseReceiver {
                broadcaster: watching.broadcaster,
                reason: None,
            });
        }
        actions
    }

    fn viewer_link(&self, viewer: &str, session_id: &str) -> Option<Link> {
        self.share
            .as_ref()?
            .viewers
            .get(viewer)
            .filter(|entry| entry.session_id == session_id)
            .map(|entry| entry.link)
    }

    fn is_watching(&self, identity: &str) -> bool {
        self.watching
            .as_ref()
            .is_some_and(|watching| watching.broadcaster == identity)
    }

    fn watching_session(&self, sender: &str, session_id: &str) -> bool {
        self.watching.as_ref().is_some_and(|watching| {
            watching.broadcaster == sender && watching.session_id == session_id
        })
    }

    fn receiver_exists(&self, sender: &str, session_id: &str) -> bool {
        self.watching_session(sender, session_id)
            && matches!(
                self.phase(),
                Some(
                    Phase::Negotiating { offered: true, .. }
                        | Phase::P2p { .. }
                        | Phase::Downgrading { .. }
                )
            )
    }

    fn phase(&self) -> Option<Phase> {
        self.watching.as_ref().map(|watching| watching.phase)
    }
}

fn send(to: &str, signal: P2pSignal) -> Action {
    Action::Send {
        to: to.to_owned(),
        signal,
    }
}

fn reject(to: &str, session_id: String, reason: Reason) -> Action {
    send(
        to,
        P2pSignal::Reject {
            session_id,
            reason: reason.as_str().to_owned(),
        },
    )
}

/// Reasons a planned downgrade can carry; anything else is reported as a bandwidth problem.
fn parse_reason(reason: &str) -> Reason {
    match reason {
        "limit_exceeded" => Reason::LimitExceeded,
        "ice_failed" => Reason::IceFailed,
        _ => Reason::BandwidthDegraded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARER: &str = "sharer";
    const ALICE: &str = "alice";
    const BOB: &str = "bob";
    const EPOCH: u64 = 42;

    fn request(session_id: &str) -> P2pSignal {
        P2pSignal::Request {
            share_epoch: EPOCH,
            session_id: session_id.to_owned(),
        }
    }

    fn offer(session_id: &str) -> P2pSignal {
        P2pSignal::Offer {
            share_epoch: EPOCH,
            session_id: session_id.to_owned(),
            sdp: "v=0".to_owned(),
        }
    }

    fn fallback(session_id: &str, reason: Reason) -> P2pSignal {
        P2pSignal::Fallback {
            session_id: session_id.to_owned(),
            reason: reason.as_str().to_owned(),
        }
    }

    fn bye(session_id: &str) -> P2pSignal {
        P2pSignal::Bye {
            session_id: session_id.to_owned(),
        }
    }

    fn rejected(to: &str, session_id: &str, reason: Reason) -> Action {
        reject(to, session_id.to_owned(), reason)
    }

    fn open(viewer: &str, session_id: &str) -> Action {
        Action::OpenSender {
            viewer: viewer.to_owned(),
            session_id: session_id.to_owned(),
            epoch: EPOCH,
        }
    }

    fn close_sender(viewer: &str, reason: Reason) -> Action {
        Action::CloseSender {
            viewer: viewer.to_owned(),
            reason,
        }
    }

    fn close_receiver(reason: Option<Reason>) -> Action {
        Action::CloseReceiver {
            broadcaster: SHARER.to_owned(),
            reason: reason.map(|reason| reason.as_str().to_owned()),
        }
    }

    fn sharing(max_viewers: usize) -> P2pState {
        let mut state = P2pState::new(true, max_viewers);
        assert!(state.share_started(EPOCH, true).is_empty());
        state
    }

    /// A viewer whose `Watch` already decodes P2P frames at `t0`.
    fn viewing_on_p2p(t0: Instant) -> P2pState {
        let mut state = P2pState::new(true, 1);
        state.watch_started(SHARER, EPOCH, "s1".to_owned(), t0);
        state.signal(SHARER, offer("s1"), t0);
        assert_eq!(
            state.tick(t0, Some(WatchMode::P2p), Some(t0)),
            vec![Action::UnsubscribeSfu {
                broadcaster: SHARER.to_owned()
            }]
        );
        state
    }

    #[test]
    fn disabled_p2p_rejects_requests_and_never_asks_to_watch_directly() {
        let t0 = Instant::now();
        let mut sharer = P2pState::new(false, 1);
        sharer.share_started(EPOCH, true);
        assert_eq!(
            sharer.signal(ALICE, request("s1"), t0),
            vec![rejected(ALICE, "s1", Reason::Disabled)]
        );

        let mut viewer = P2pState::new(false, 1);
        assert!(
            viewer
                .watch_started(SHARER, EPOCH, "s1".to_owned(), t0)
                .is_empty()
        );
        assert!(viewer.signal(SHARER, offer("s1"), t0).is_empty());
        assert!(viewer.tick(t0 + CONNECT_TIMEOUT, None, None).is_empty());
        assert!(viewer.watch_stopped().is_empty());
    }

    #[test]
    fn sharer_checks_epoch_gpu_path_and_rate_limit_before_opening_a_peer() {
        let t0 = Instant::now();
        let mut state = P2pState::new(true, 1);
        assert_eq!(
            state.signal(ALICE, request("s0"), t0),
            vec![rejected(ALICE, "s0", Reason::StaleEpoch)]
        );

        state.share_started(EPOCH, false);
        assert_eq!(
            state.signal(ALICE, request("s1"), t0),
            vec![rejected(ALICE, "s1", Reason::CpuFallback)]
        );

        let mut state = sharing(1);
        assert_eq!(
            state.signal(
                ALICE,
                P2pSignal::Request {
                    share_epoch: EPOCH + 1,
                    session_id: "s1".to_owned()
                },
                t0
            ),
            vec![rejected(ALICE, "s1", Reason::StaleEpoch)]
        );
        assert_eq!(
            state.signal(ALICE, request("s1"), t0),
            vec![open(ALICE, "s1")]
        );
        let again = t0 + Duration::from_secs(1);
        assert_eq!(
            state.signal(ALICE, request("s2"), again),
            vec![
                close_sender(ALICE, Reason::ViewerLeft),
                rejected(ALICE, "s2", Reason::RateLimited)
            ]
        );
        assert_eq!(
            state.signal(ALICE, request("s3"), t0 + REQUEST_COOLDOWN),
            vec![open(ALICE, "s3")]
        );
    }

    #[test]
    fn sharer_routes_answer_and_ice_only_to_the_matching_session() {
        let t0 = Instant::now();
        let mut state = sharing(1);
        state.signal(ALICE, request("s1"), t0);
        let ice = P2pSignal::Ice {
            session_id: "s1".to_owned(),
            candidate: "candidate:1".to_owned(),
            sdp_mid: "0".to_owned(),
            sdp_mline_index: 0,
        };
        assert_eq!(
            state.signal(
                ALICE,
                P2pSignal::Answer {
                    session_id: "s1".to_owned(),
                    sdp: "v=0".to_owned()
                },
                t0
            ),
            vec![Action::AcceptAnswer {
                viewer: ALICE.to_owned(),
                sdp: "v=0".to_owned()
            }]
        );
        assert!(matches!(
            state.signal(ALICE, ice.clone(), t0).as_slice(),
            [Action::SenderIce { viewer, .. }] if viewer == ALICE
        ));
        assert!(state.signal(BOB, ice, t0).is_empty());
        assert!(
            state
                .signal(
                    ALICE,
                    P2pSignal::Answer {
                        session_id: "other".to_owned(),
                        sdp: "v=0".to_owned()
                    },
                    t0
                )
                .is_empty()
        );
    }

    #[test]
    fn second_viewer_over_the_limit_sends_everyone_to_the_sfu() {
        let t0 = Instant::now();
        let mut state = sharing(1);
        assert_eq!(
            state.signal(ALICE, request("a1"), t0),
            vec![open(ALICE, "a1")]
        );
        assert_eq!(
            state.signal(BOB, request("b1"), t0),
            vec![
                rejected(BOB, "b1", Reason::LimitExceeded),
                send(ALICE, fallback("a1", Reason::LimitExceeded)),
            ]
        );
        assert!(
            state
                .tick(t0 + Duration::from_secs(1), None, None)
                .is_empty()
        );
        assert_eq!(
            state.tick(t0 + DOWNGRADE_GRACE, None, None),
            vec![close_sender(ALICE, Reason::LimitExceeded)]
        );
        // Bob stops watching; the limit is free again for whoever asks next.
        assert!(state.signal(BOB, bye("b1"), t0).is_empty());
        let later = t0 + REQUEST_COOLDOWN;
        assert_eq!(
            state.signal(ALICE, request("a2"), later),
            vec![open(ALICE, "a2")]
        );
    }

    #[test]
    fn zero_viewer_limit_keeps_everyone_on_the_sfu() {
        let t0 = Instant::now();
        let mut state = sharing(0);
        assert_eq!(
            state.signal(ALICE, request("a1"), t0),
            vec![rejected(ALICE, "a1", Reason::LimitExceeded)]
        );
    }

    #[test]
    fn bandwidth_degradation_and_peer_failure_close_the_sender_side() {
        let t0 = Instant::now();
        let mut state = sharing(2);
        state.signal(ALICE, request("a1"), t0);
        state.signal(BOB, request("b1"), t0);
        assert_eq!(
            state.bandwidth_degraded(t0),
            vec![
                send(ALICE, fallback("a1", Reason::BandwidthDegraded)),
                send(BOB, fallback("b1", Reason::BandwidthDegraded)),
            ]
        );
        assert_eq!(
            state.signal(ALICE, fallback("a1", Reason::FrameTimeout), t0),
            vec![close_sender(ALICE, Reason::ViewerFallback)]
        );
        assert_eq!(
            state.tick(t0 + DOWNGRADE_GRACE, None, None),
            vec![close_sender(BOB, Reason::BandwidthDegraded)]
        );

        let mut state = sharing(1);
        state.signal(ALICE, request("a1"), t0);
        assert_eq!(
            state.sender_failed(ALICE, "a1"),
            vec![
                close_sender(ALICE, Reason::IceFailed),
                send(ALICE, fallback("a1", Reason::IceFailed)),
            ]
        );
        assert!(state.sender_failed(ALICE, "a1").is_empty());
    }

    #[test]
    fn stopping_the_share_or_the_viewer_leaving_closes_every_sender_peer() {
        let t0 = Instant::now();
        let mut state = sharing(2);
        state.signal(ALICE, request("a1"), t0);
        state.signal(BOB, request("b1"), t0);
        assert_eq!(
            state.participant_left(BOB),
            vec![close_sender(BOB, Reason::ViewerLeft)]
        );
        assert_eq!(
            state.share_stopped(),
            vec![
                send(ALICE, bye("a1")),
                close_sender(ALICE, Reason::ShareStopped)
            ]
        );
        assert!(state.share_stopped().is_empty());
        assert_eq!(
            state.signal(ALICE, request("a2"), t0 + REQUEST_COOLDOWN),
            vec![rejected(ALICE, "a2", Reason::StaleEpoch)]
        );

        let mut state = sharing(1);
        state.signal(ALICE, request("a1"), t0);
        assert_eq!(
            state.epoch_changed(EPOCH + 1),
            vec![
                send(ALICE, bye("a1")),
                close_sender(ALICE, Reason::ShareStopped)
            ]
        );
        assert!(state.epoch_changed(EPOCH + 1).is_empty());
        assert_eq!(
            state.signal(ALICE, bye("a1"), t0),
            Vec::<Action>::new(),
            "a stale session cannot touch the new epoch"
        );
    }

    #[test]
    fn viewer_upgrades_only_after_the_watch_shows_a_p2p_frame() {
        let t0 = Instant::now();
        let mut state = P2pState::new(true, 1);
        assert_eq!(
            state.watch_started(SHARER, EPOCH, "s1".to_owned(), t0),
            vec![send(SHARER, request("s1"))]
        );
        assert!(state.signal(BOB, offer("s1"), t0).is_empty());
        assert!(state.signal(SHARER, offer("other"), t0).is_empty());
        assert_eq!(
            state.signal(SHARER, offer("s1"), t0),
            vec![Action::OpenReceiver {
                broadcaster: SHARER.to_owned(),
                session_id: "s1".to_owned(),
                sdp: "v=0".to_owned()
            }]
        );
        assert!(state.signal(SHARER, offer("s1"), t0).is_empty());
        let t1 = t0 + Duration::from_secs(1);
        assert!(
            state
                .tick(t1, Some(WatchMode::UpgradingToP2p), Some(t1))
                .is_empty()
        );
        assert_eq!(
            state.tick(t1, Some(WatchMode::P2p), Some(t1)),
            vec![Action::UnsubscribeSfu {
                broadcaster: SHARER.to_owned()
            }]
        );
        assert!(
            state
                .tick(t1 + FRAME_STALL_TIMEOUT, Some(WatchMode::P2p), Some(t1))
                .is_empty()
        );
    }

    #[test]
    fn viewer_falls_back_on_rejection_or_nat_timeout() {
        let t0 = Instant::now();
        let mut state = P2pState::new(true, 1);
        state.watch_started(SHARER, EPOCH, "s1".to_owned(), t0);
        assert_eq!(
            state.signal(SHARER, rejected_signal("s1", "limit_exceeded"), t0),
            vec![Action::CloseReceiver {
                broadcaster: SHARER.to_owned(),
                reason: Some("limit_exceeded".to_owned())
            }]
        );
        assert!(state.tick(t0 + CONNECT_TIMEOUT, None, None).is_empty());

        let mut state = P2pState::new(true, 1);
        state.watch_started(SHARER, EPOCH, "s2".to_owned(), t0);
        state.signal(SHARER, offer("s2"), t0);
        assert!(
            state
                .tick(t0 + Duration::from_secs(9), Some(WatchMode::Sfu), None)
                .is_empty()
        );
        assert_eq!(
            state.tick(t0 + CONNECT_TIMEOUT, Some(WatchMode::Sfu), None),
            vec![
                send(SHARER, fallback("s2", Reason::NatTimeout)),
                close_receiver(Some(Reason::NatTimeout)),
            ]
        );
    }

    fn rejected_signal(session_id: &str, reason: &str) -> P2pSignal {
        P2pSignal::Reject {
            session_id: session_id.to_owned(),
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn viewer_fails_over_abruptly_on_frame_stall_disconnect_or_peer_failure() {
        let t0 = Instant::now();
        let mut state = viewing_on_p2p(t0);
        let stalled = t0 + FRAME_STALL_TIMEOUT + Duration::from_millis(1);
        assert_eq!(
            state.tick(stalled, Some(WatchMode::P2p), Some(t0)),
            vec![
                send(SHARER, fallback("s1", Reason::FrameTimeout)),
                close_receiver(Some(Reason::FrameTimeout)),
            ]
        );
        assert!(
            state
                .tick(stalled, Some(WatchMode::Sfu), Some(t0))
                .is_empty()
        );

        let mut state = viewing_on_p2p(t0);
        let t1 = t0 + Duration::from_secs(1);
        state.receiver_state("s1", P2pPeerState::Disconnected, t1);
        state.receiver_state("s1", P2pPeerState::Connected, t1);
        state.receiver_state("s1", P2pPeerState::Disconnected, t1);
        let t3 = t1 + DISCONNECTED_TIMEOUT;
        assert_eq!(
            state.tick(t3, Some(WatchMode::P2p), Some(t3)),
            vec![
                send(SHARER, fallback("s1", Reason::IceFailed)),
                close_receiver(Some(Reason::IceFailed)),
            ]
        );

        let mut state = viewing_on_p2p(t0);
        assert!(
            state
                .receiver_state("other", P2pPeerState::Failed, t0)
                .is_empty()
        );
        assert_eq!(
            state.receiver_state("s1", P2pPeerState::Failed, t0),
            vec![
                send(SHARER, fallback("s1", Reason::IceFailed)),
                close_receiver(Some(Reason::IceFailed)),
            ]
        );
    }

    #[test]
    fn planned_downgrade_keeps_p2p_until_the_sfu_frame_or_the_grace_timeout() {
        let t0 = Instant::now();
        let mut state = viewing_on_p2p(t0);
        assert_eq!(
            state.signal(SHARER, fallback("s1", Reason::LimitExceeded), t0),
            vec![Action::BeginDowngrade {
                broadcaster: SHARER.to_owned(),
                reason: Reason::LimitExceeded
            }]
        );
        let t1 = t0 + Duration::from_secs(1);
        assert!(
            state
                .tick(t1, Some(WatchMode::DowngradingToSfu), Some(t0))
                .is_empty()
        );
        assert_eq!(
            state.tick(t1, Some(WatchMode::Sfu), Some(t0)),
            vec![close_receiver(None)]
        );

        let mut state = viewing_on_p2p(t0);
        state.signal(SHARER, fallback("s1", Reason::BandwidthDegraded), t0);
        assert_eq!(
            state.tick(
                t0 + DOWNGRADE_GRACE,
                Some(WatchMode::DowngradingToSfu),
                Some(t0)
            ),
            vec![close_receiver(None)]
        );
    }

    #[test]
    fn stopping_or_switching_the_watch_ends_the_old_session_cleanly() {
        let t0 = Instant::now();
        let mut state = viewing_on_p2p(t0);
        assert_eq!(
            state.watch_started(SHARER, EPOCH, "s2".to_owned(), t0),
            vec![
                send(SHARER, bye("s1")),
                close_receiver(None),
                send(SHARER, request("s2"))
            ]
        );
        assert_eq!(
            state.watch_stopped(),
            vec![send(SHARER, bye("s2")), close_receiver(None)]
        );
        assert!(state.watch_stopped().is_empty());

        let mut state = viewing_on_p2p(t0);
        assert_eq!(
            state.signal(SHARER, bye("s1"), t0),
            vec![close_receiver(Some(Reason::ShareStopped))]
        );
        let mut state = viewing_on_p2p(t0);
        assert_eq!(
            state.remote_share_stopped(SHARER),
            vec![send(SHARER, bye("s1")), close_receiver(None)]
        );
        let mut state = viewing_on_p2p(t0);
        assert_eq!(state.participant_left(SHARER), vec![close_receiver(None)]);
    }
}
