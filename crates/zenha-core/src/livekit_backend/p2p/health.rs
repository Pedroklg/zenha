//! Saúde da banda do P2P (histerese antes de voltar ao SFU) e exportação das estatísticas em CSV.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::{self, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use livekit::webrtc::stats::QualityLimitationReason;

use super::{P2pRole, P2pStatsSnapshot};

/// Below half of the preset bitrate the picture of a 1080p game falls apart.
const BITRATE_FLOOR_RATIO: f64 = 0.5;
const MAX_FRACTION_LOST: f64 = 0.05;
/// GCC starts at half the preset and ramps up; do not judge the link before that.
const WARMUP: Duration = Duration::from_secs(3);
const HYSTERESIS: Duration = Duration::from_secs(5);
const CSV_QUEUE: usize = 256;

const CSV_HEADER: &str = "unix_ms,role,peer,session_id,event,reason,route,bytes_sent,bytes_received,bitrate_bps,available_outgoing_bitrate_bps,target_bitrate_bps,rtt_ms,fraction_lost,packets_sent,packets_received,packets_lost,retransmitted_packets_sent,packets_discarded_on_send,nack_count,pli_count,frames_sent,frames_encoded,frames_received,frames_decoded,frames_dropped,freeze_count,total_freeze_duration_s,jitter_ms,frame_width,frame_height,fps,quality_limitation_reason,active_p2p_viewers";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Degradation {
    LowBitrate,
    PacketLoss,
    SendDiscards,
}

impl Degradation {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::LowBitrate => "low_bitrate",
            Self::PacketLoss => "packet_loss",
            Self::SendDiscards => "send_discards",
        }
    }
}

/// Judges the sender's 1 s stats; a problem must last [`HYSTERESIS`] before it counts.
#[derive(Debug)]
pub(super) struct HealthMonitor {
    floor_bps: f64,
    connected_at: Instant,
    degraded_since: Option<Instant>,
    last_discarded: Option<u32>,
}

impl HealthMonitor {
    pub(super) fn new(preset_bitrate_bps: u64, connected_at: Instant) -> Self {
        Self {
            floor_bps: preset_bitrate_bps as f64 * BITRATE_FLOOR_RATIO,
            connected_at,
            degraded_since: None,
            last_discarded: None,
        }
    }

    /// Returns the cause once the link stayed degraded for the whole hysteresis window.
    pub(super) fn evaluate(
        &mut self,
        stats: &P2pStatsSnapshot,
        now: Instant,
    ) -> Option<Degradation> {
        let discards_grew = self
            .last_discarded
            .is_some_and(|last| stats.packets_discarded_on_send > last);
        self.last_discarded = Some(stats.packets_discarded_on_send);
        let cause = if now.saturating_duration_since(self.connected_at) < WARMUP {
            None
        } else {
            self.degradation(stats, discards_grew)
        };
        let Some(cause) = cause else {
            self.degraded_since = None;
            return None;
        };
        let since = *self.degraded_since.get_or_insert(now);
        (now.saturating_duration_since(since) >= HYSTERESIS).then_some(cause)
    }

    fn degradation(&self, stats: &P2pStatsSnapshot, discards_grew: bool) -> Option<Degradation> {
        let below_floor = |bps: Option<f64>| {
            bps.is_some_and(|bps| bps.is_finite() && bps > 0.0 && bps < self.floor_bps)
        };
        // A static screen lowers the target without any network trouble, so the target only
        // counts when the encoder says bandwidth is what limits it.
        let low_target = below_floor(stats.target_bitrate_bps)
            && (stats.available_outgoing_bitrate_bps.is_none()
                || stats.quality_limitation_reason == QualityLimitationReason::Bandwidth);
        if below_floor(stats.available_outgoing_bitrate_bps) || low_target {
            return Some(Degradation::LowBitrate);
        }
        if stats
            .fraction_lost
            .is_some_and(|loss| loss > MAX_FRACTION_LOST)
        {
            return Some(Degradation::PacketLoss);
        }
        discards_grew.then_some(Degradation::SendDiscards)
    }
}

pub(super) struct CsvRow<'a> {
    pub(super) role: P2pRole,
    pub(super) peer: &'a str,
    pub(super) session_id: &'a str,
    pub(super) event: &'a str,
    pub(super) reason: &'a str,
    pub(super) active_p2p_viewers: usize,
}

fn csv_field(value: &str) -> String {
    value
        .chars()
        .filter(|&c| c != ',' && c != '"' && !c.is_control())
        .collect()
}

fn push_opt(row: &mut String, value: Option<f64>, decimals: usize) {
    row.push(',');
    if let Some(value) = value.filter(|value| value.is_finite()) {
        let _ = write!(row, "{value:.decimals$}");
    }
}

fn quality_limitation(reason: QualityLimitationReason) -> &'static str {
    match reason {
        QualityLimitationReason::None => "none",
        QualityLimitationReason::Cpu => "cpu",
        QualityLimitationReason::Bandwidth => "bandwidth",
        QualityLimitationReason::Other => "other",
    }
}

fn format_row(unix_ms: u128, row: &CsvRow<'_>, s: &P2pStatsSnapshot) -> String {
    let mut out = format!(
        "{unix_ms},{},{},{},{},{},{},{},{}",
        row.role.as_str(),
        csv_field(row.peer),
        csv_field(row.session_id),
        csv_field(row.event),
        csv_field(row.reason),
        csv_field(s.route.as_deref().unwrap_or_default()),
        s.bytes_sent,
        s.bytes_received,
    );
    push_opt(&mut out, s.bitrate_bps, 0);
    push_opt(&mut out, s.available_outgoing_bitrate_bps, 0);
    push_opt(&mut out, s.target_bitrate_bps, 0);
    push_opt(&mut out, s.rtt_ms, 2);
    push_opt(&mut out, s.fraction_lost, 4);
    let _ = write!(
        out,
        ",{},{},{},{},{},{},{},{},{},{},{},{},{},{:.3}",
        s.packets_sent,
        s.packets_received,
        s.packets_lost,
        s.retransmitted_packets_sent,
        s.packets_discarded_on_send,
        s.nack_count,
        s.pli_count,
        s.frames_sent,
        s.frames_encoded,
        s.frames_received,
        s.frames_decoded,
        s.frames_dropped,
        s.freeze_count,
        s.total_freeze_duration_s,
    );
    push_opt(&mut out, s.jitter_ms, 2);
    let _ = write!(
        out,
        ",{},{},{:.2},{},{}",
        s.frame_width,
        s.frame_height,
        s.frames_per_second,
        quality_limitation(s.quality_limitation_reason),
        row.active_p2p_viewers,
    );
    out
}

/// `ZENHA_P2P_STATS_LOG`: rows go to a writer thread so the Tokio loop never waits on the disk.
pub(super) struct StatsCsv {
    tx: Option<SyncSender<String>>,
    thread: Option<JoinHandle<()>>,
}

impl StatsCsv {
    pub(super) fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let empty = file.metadata()?.len() == 0;
        let mut writer = BufWriter::new(file);
        if empty {
            writeln!(writer, "{CSV_HEADER}")?;
        }
        let (tx, rx) = mpsc::sync_channel::<String>(CSV_QUEUE);
        let thread = std::thread::Builder::new()
            .name("p2p-stats-csv".to_owned())
            .spawn(move || {
                for line in rx {
                    if writeln!(writer, "{line}")
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    /// Drops the row when the writer falls behind.
    pub(super) fn record(&self, row: &CsvRow<'_>, stats: &P2pStatsSnapshot) {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(format_row(unix_ms, row, stats));
        }
    }
}

impl Drop for StatsCsv {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESET_BPS: u64 = 8_000_000;

    fn healthy() -> P2pStatsSnapshot {
        P2pStatsSnapshot {
            available_outgoing_bitrate_bps: Some(7_000_000.0),
            target_bitrate_bps: Some(7_000_000.0),
            fraction_lost: Some(0.01),
            ..Default::default()
        }
    }

    fn at(t0: Instant, secs: u64) -> Instant {
        t0 + Duration::from_secs(secs)
    }

    #[test]
    fn low_bitrate_must_last_the_whole_window_after_warmup() {
        let t0 = Instant::now();
        let mut monitor = HealthMonitor::new(PRESET_BPS, t0);
        let low = P2pStatsSnapshot {
            available_outgoing_bitrate_bps: Some(3_000_000.0),
            ..healthy()
        };
        for secs in 0..WARMUP.as_secs() {
            assert_eq!(monitor.evaluate(&low, at(t0, secs)), None);
        }
        for secs in 3..8 {
            assert_eq!(monitor.evaluate(&low, at(t0, secs)), None, "t={secs}");
        }
        assert_eq!(monitor.evaluate(&healthy(), at(t0, 8)), None);
        for secs in 9..14 {
            assert_eq!(monitor.evaluate(&low, at(t0, secs)), None, "t={secs}");
        }
        assert_eq!(
            monitor.evaluate(&low, at(t0, 14)),
            Some(Degradation::LowBitrate)
        );
    }

    #[test]
    fn sustained_loss_or_growing_send_discards_trigger_the_fallback() {
        let t0 = Instant::now();
        let mut loss = HealthMonitor::new(PRESET_BPS, t0);
        let lossy = P2pStatsSnapshot {
            fraction_lost: Some(0.08),
            ..healthy()
        };
        assert_eq!(loss.evaluate(&lossy, at(t0, 4)), None);
        assert_eq!(
            loss.evaluate(&lossy, at(t0, 9)),
            Some(Degradation::PacketLoss)
        );

        let mut discards = HealthMonitor::new(PRESET_BPS, t0);
        let with = |count| P2pStatsSnapshot {
            packets_discarded_on_send: count,
            ..healthy()
        };
        assert_eq!(discards.evaluate(&with(0), at(t0, 3)), None);
        for (secs, count) in (4..9).zip(1..) {
            assert_eq!(discards.evaluate(&with(count), at(t0, secs)), None);
        }
        assert_eq!(
            discards.evaluate(&with(6), at(t0, 9)),
            Some(Degradation::SendDiscards)
        );
        assert_eq!(discards.evaluate(&with(6), at(t0, 10)), None);
    }

    #[test]
    fn low_target_counts_only_when_bandwidth_limits_the_encoder() {
        let t0 = Instant::now();
        let mut limited = HealthMonitor::new(PRESET_BPS, t0);
        let no_bwe = P2pStatsSnapshot {
            target_bitrate_bps: Some(3_000_000.0),
            ..Default::default()
        };
        assert_eq!(limited.evaluate(&no_bwe, at(t0, 4)), None);
        assert_eq!(
            limited.evaluate(&no_bwe, at(t0, 9)),
            Some(Degradation::LowBitrate)
        );

        let mut static_screen = HealthMonitor::new(PRESET_BPS, t0);
        let calm = P2pStatsSnapshot {
            available_outgoing_bitrate_bps: Some(9_000_000.0),
            target_bitrate_bps: Some(2_000_000.0),
            quality_limitation_reason: QualityLimitationReason::None,
            ..Default::default()
        };
        assert_eq!(static_screen.evaluate(&calm, at(t0, 4)), None);
        assert_eq!(static_screen.evaluate(&calm, at(t0, 20)), None);
    }

    #[test]
    fn csv_rows_match_the_header_and_reach_the_file() {
        let stats = P2pStatsSnapshot {
            bytes_sent: 120_000,
            bitrate_bps: Some(3_000_000.0),
            fraction_lost: Some(0.0625),
            jitter_ms: Some(3.2),
            route: Some("ipv4_srflx".to_owned()),
            ..Default::default()
        };
        let row = CsvRow {
            role: P2pRole::Sender,
            peer: "v,1",
            session_id: "s\"1",
            event: "fallback",
            reason: "bandwidth_degraded",
            active_p2p_viewers: 2,
        };
        let line = format_row(1_700_000_000_000, &row, &stats);
        assert_eq!(
            line.split(',').count(),
            CSV_HEADER.split(',').count(),
            "{line}"
        );
        assert!(line.starts_with(
            "1700000000000,sender,v1,s1,fallback,bandwidth_degraded,ipv4_srflx,120000,0,3000000,"
        ));
        assert!(line.ends_with(",none,2"));

        let path = std::env::temp_dir().join(format!("zenha-p2p-{}.csv", uuid::Uuid::new_v4()));
        {
            let csv = StatsCsv::open(&path).unwrap();
            csv.record(&row, &stats);
            csv.record(
                &CsvRow {
                    event: "stats",
                    reason: "",
                    ..row
                },
                &stats,
            );
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], CSV_HEADER);
        assert!(lines[1].contains(",fallback,bandwidth_degraded,"));
        assert!(lines[2].contains(",stats,,"));
    }
}
