//! Sends the app's own warnings and errors, and a crash of the previous run, to the server.
//! The sender thread sleeps on a channel and only wakes when something went wrong.

use std::collections::{HashMap, VecDeque};
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use zenha_protocol::{
    ClientReport, MAX_DISPLAY_NAME_LEN, MAX_REPORT_EVENTS, MAX_REPORT_MESSAGE_LEN,
    MAX_REPORT_META_LEN, MAX_REPORT_TARGET_LEN, ReportEvent, ReportLevel,
};

use super::{credentials, settings};

const QUEUE: usize = 256;
const STARTUP_DELAY: Duration = Duration::from_secs(20);
/// Waits after the first event so a burst goes out as one report.
const BATCH_DELAY: Duration = Duration::from_secs(10);
const MIN_INTERVAL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const NO_PREVIOUS_RUN_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
const PANIC_MARKER: &str = "zenha::platform::logging: panic";
/// libwebrtc's audio device messages; they say why the device could not start.
const WEBRTC_AUDIO_PREFIX: &str = "(audio_device";
const AUDIO_CONTEXT_LEN: usize = 10;
const AUDIO_CONTEXT_MAX_AGE_MS: u64 = 60_000;

/// What is known about the run before this one, to report how it ended.
pub struct PreviousRun {
    pub log: Option<PathBuf>,
    pub started: Option<SystemTime>,
}

pub struct ReportLayer {
    tx: SyncSender<ReportEvent>,
    /// Recent libwebrtc audio device messages, sent along with the next report.
    audio_context: Mutex<VecDeque<ReportEvent>>,
}

impl ReportLayer {
    fn new(tx: SyncSender<ReportEvent>) -> Self {
        Self {
            tx,
            audio_context: Mutex::new(VecDeque::new()),
        }
    }

    fn queue(&self, event: ReportEvent) {
        let mut context = self
            .audio_context
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let fresh_after = event.at_unix_ms.saturating_sub(AUDIO_CONTEXT_MAX_AGE_MS);
        // A full queue drops the event: reporting must never block logging.
        for line in context
            .drain(..)
            .filter(|line| line.at_unix_ms >= fresh_after)
        {
            let _ = self.tx.try_send(line);
        }
        let _ = self.tx.try_send(event);
    }

    fn remember_audio(&self, event: ReportEvent) {
        let mut context = self
            .audio_context
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if context.len() == AUDIO_CONTEXT_LEN {
            context.pop_front();
        }
        context.push_back(event);
    }
}

/// Starts the sender thread and returns the layer that feeds it.
pub fn start(previous: PreviousRun) -> Option<ReportLayer> {
    let (tx, rx) = sync_channel(QUEUE);
    thread::Builder::new()
        .name("error-report".into())
        .spawn(move || run(&rx, &previous))
        .ok()?;
    Some(ReportLayer::new(tx))
}

impl<S: Subscriber> Layer<S> for ReportLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = match *meta.level() {
            Level::ERROR => ReportLevel::Error,
            Level::WARN => ReportLevel::Warn,
            _ => return,
        };
        let mut text = EventText::default();
        event.record(&mut text);
        let target = text.log_target.as_deref().unwrap_or(meta.target());
        let is_app = target.starts_with("zenha");
        let is_webrtc_audio = target == "libwebrtc" && text.text.starts_with(WEBRTC_AUDIO_PREFIX);
        if !is_app && !is_webrtc_audio {
            return;
        }
        let report = ReportEvent {
            at_unix_ms: now_unix_ms(),
            level,
            target: truncate(target, MAX_REPORT_TARGET_LEN),
            message: clean(&text.text),
            count: 1,
        };
        if is_app || (level == ReportLevel::Error && !is_known_webrtc_audio_noise(&report.message))
        {
            self.queue(report);
        } else {
            self.remember_audio(report);
        }
    }
}

/// Errors libwebrtc logs on every normal start of the built-in echo canceller.
fn is_known_webrtc_audio_noise(message: &str) -> bool {
    message.contains(" AEC")
}

#[derive(Default)]
struct EventText {
    text: String,
    /// Real target of an event bridged from the `log` crate.
    log_target: Option<String>,
}

impl EventText {
    fn push(&mut self, field: &Field, value: &dyn fmt::Display) {
        match field.name() {
            "backtrace" => {}
            "log.target" => self.log_target = Some(value.to_string()),
            name if name.starts_with("log.") => {}
            "message" => {
                let rest = std::mem::take(&mut self.text);
                let _ = write!(self.text, "{value}{rest}");
            }
            name => {
                let _ = write!(self.text, " {name}={value}");
            }
        }
    }
}

impl Visit for EventText {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, &value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, &format_args!("{value:?}"));
    }
}

fn run(rx: &Receiver<ReportEvent>, previous: &PreviousRun) {
    thread::sleep(STARTUP_DELAY);
    let mut pending = previous_run_events(previous);
    loop {
        if pending.is_empty() {
            match rx.recv() {
                Ok(event) => pending.push(event),
                Err(_) => return,
            }
            thread::sleep(BATCH_DELAY);
        }
        pending.extend(rx.try_iter());
        send(merge(std::mem::take(&mut pending)));
        thread::sleep(MIN_INTERVAL);
    }
}

/// Folds repeats of the same event into one with a count, keeping the order of first sight.
fn merge(events: Vec<ReportEvent>) -> Vec<ReportEvent> {
    let mut merged: Vec<ReportEvent> = Vec::new();
    let mut index: HashMap<(ReportLevel, String, String), usize> = HashMap::new();
    for event in events {
        let key = (event.level, event.target.clone(), event.message.clone());
        if let Some(&at) = index.get(&key) {
            merged[at].count = merged[at].count.saturating_add(event.count);
        } else if merged.len() < MAX_REPORT_EVENTS {
            index.insert(key, merged.len());
            merged.push(event);
        }
    }
    merged
}

fn send(events: Vec<ReportEvent>) {
    if events.is_empty() {
        return;
    }
    let Some(path) = settings::default_path() else {
        return;
    };
    let settings = settings::load(&path);
    let server = settings
        .token_server_url
        .trim()
        .trim_end_matches('/')
        .to_owned();
    if server.is_empty() || settings.client_id.is_empty() {
        return;
    }
    let Some(invite) = credentials::load(&server) else {
        return;
    };
    let report = ClientReport {
        client_id: settings.client_id,
        display_name: truncate(settings.display_name.trim(), MAX_DISPLAY_NAME_LEN),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        os: truncate(&os_version(), MAX_REPORT_META_LEN),
        events,
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    zenha_core::install_crypto_provider();
    let result = runtime.block_on(async {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?
            .post(format!("{server}/client-reports"))
            .header("x-invite-code", invite)
            .json(&report)
            .send()
            .await?
            .error_for_status()
    });
    match result {
        Ok(_) => tracing::debug!(events = report.events.len(), "error report sent"),
        Err(err) => tracing::debug!(%err, "error report not sent"),
    }
}

fn previous_run_events(previous: &PreviousRun) -> Vec<ReportEvent> {
    let mut events = Vec::new();
    if let Some(line) = previous
        .log
        .as_deref()
        .and_then(|log| std::fs::read_to_string(log).ok())
        .and_then(|text| panic_line(&text))
    {
        events.push(crash_event(format!("previous run panicked: {line}")));
    }
    let window = previous
        .started
        .and_then(|started| started.elapsed().ok())
        .unwrap_or(NO_PREVIOUS_RUN_WINDOW);
    events.extend(
        native_crashes(&query_crash_events(window))
            .into_iter()
            .map(crash_event),
    );
    events
}

fn crash_event(message: String) -> ReportEvent {
    ReportEvent {
        at_unix_ms: now_unix_ms(),
        level: ReportLevel::Crash,
        target: "zenha::previous_run".to_owned(),
        message: clean(&message),
        count: 1,
    }
}

fn panic_line(log: &str) -> Option<String> {
    let line = log.lines().find(|line| line.contains(PANIC_MARKER))?;
    let line = line.split(" backtrace=").next().unwrap_or(line);
    Some(line.trim().to_owned())
}

/// `Application Error` (event 1000) entries for zenha.exe from `wevtutil` XML output.
fn native_crashes(xml: &str) -> Vec<String> {
    xml.split("</Event>")
        .filter_map(|event| {
            let data: Vec<&str> = event
                .split("<Data>")
                .skip(1)
                .filter_map(|part| part.split("</Data>").next())
                .collect();
            if !data.first()?.eq_ignore_ascii_case("zenha.exe") {
                return None;
            }
            let when = event
                .split("SystemTime='")
                .nth(1)
                .and_then(|rest| rest.split('\'').next())
                .unwrap_or("?");
            Some(format!(
                "previous run crashed at {when}: exception {} in {}+{} (version {})",
                data.get(6).unwrap_or(&"?"),
                data.get(3).unwrap_or(&"?"),
                data.get(7).unwrap_or(&"?"),
                data.get(1).unwrap_or(&"?"),
            ))
        })
        .collect()
}

#[cfg(windows)]
fn query_crash_events(window: Duration) -> String {
    let query = format!(
        "*[System[Provider[@Name='Application Error'] and (EventID=1000) and TimeCreated[timediff(@SystemTime) <= {}]]]",
        window.as_millis()
    );
    let query_arg = format!("/q:{query}");
    run_hidden(
        "wevtutil",
        &[
            "qe",
            "Application",
            &query_arg,
            "/f:xml",
            "/c:10",
            "/rd:true",
        ],
    )
}

#[cfg(not(windows))]
fn query_crash_events(_window: Duration) -> String {
    String::new()
}

#[cfg(windows)]
fn os_version() -> String {
    let ver = run_hidden("cmd", &["/c", "ver"]);
    let version: String = ver
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    format!("Windows {version}")
}

#[cfg(not(windows))]
fn os_version() -> String {
    std::env::consts::OS.to_owned()
}

#[cfg(windows)]
fn run_hidden(program: &str, args: &[&str]) -> String {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

/// Hides the Windows user name in paths and caps the length.
fn clean(text: &str) -> String {
    let text = match std::env::var("USERPROFILE") {
        Ok(profile) if !profile.is_empty() => replace_ignore_case(text, &profile, "%USERPROFILE%"),
        _ => text.to_owned(),
    };
    truncate(text.trim(), MAX_REPORT_MESSAGE_LEN)
}

fn replace_ignore_case(text: &str, needle: &str, with: &str) -> String {
    let lower_text = text.to_lowercase();
    let lower_needle = needle.to_lowercase();
    if lower_text.len() != text.len() || lower_needle.len() != needle.len() {
        return text.replace(needle, with);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = 0;
    for (at, _) in lower_text.match_indices(&lower_needle) {
        out.push_str(&text[rest..at]);
        out.push_str(with);
        rest = at + needle.len();
    }
    out.push_str(&text[rest..]);
    out
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Start time of a run from its log name, `zenha-<unix secs>-<pid>.log`.
pub fn run_started(log: &Path) -> Option<SystemTime> {
    let name = log.file_name()?.to_str()?;
    let secs: u64 = name
        .strip_prefix("zenha-")?
        .split('-')
        .next()?
        .parse()
        .ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(message: &str) -> ReportEvent {
        ReportEvent {
            at_unix_ms: 1,
            level: ReportLevel::Warn,
            target: "zenha_core::audio".to_owned(),
            message: message.to_owned(),
            count: 1,
        }
    }

    #[test]
    fn merge_counts_repeats_and_keeps_first_order() {
        let merged = merge(vec![event("a"), event("b"), event("a"), event("a")]);
        assert_eq!(merged.len(), 2);
        assert_eq!((merged[0].message.as_str(), merged[0].count), ("a", 3));
        assert_eq!((merged[1].message.as_str(), merged[1].count), ("b", 1));
    }

    #[test]
    fn merge_caps_distinct_events() {
        let events = (0..MAX_REPORT_EVENTS + 5)
            .map(|i| event(&i.to_string()))
            .collect();
        assert_eq!(merge(events).len(), MAX_REPORT_EVENTS);
    }

    #[test]
    fn panic_line_drops_the_backtrace() {
        let log = "2026-11-20T10:00:00Z  INFO zenha: zenha starting\n\
                   2026-11-20T10:00:01Z ERROR zenha::platform::logging: panic info=panicked at src/x.rs:1:1 backtrace=   0: std::backtrace";
        assert_eq!(
            panic_line(log).as_deref(),
            Some(
                "2026-11-20T10:00:01Z ERROR zenha::platform::logging: panic info=panicked at src/x.rs:1:1"
            )
        );
        assert_eq!(panic_line("all fine"), None);
    }

    #[test]
    fn native_crashes_reads_zenha_entries_only() {
        let xml = "<Event><System><TimeCreated SystemTime='2026-11-20T21:56:20.1Z'/></System><EventData>\
                   <Data>zenha.exe</Data><Data>0.1.17.0</Data><Data>6b00bc44</Data><Data>zenha.exe</Data>\
                   <Data>0.1.17.0</Data><Data>6b00bc44</Data><Data>c0000409</Data><Data>000000000355fc35</Data>\
                   </EventData></Event><Event><EventData><Data>other.exe</Data></EventData></Event>";
        assert_eq!(
            native_crashes(xml),
            vec![
                "previous run crashed at 2026-11-20T21:56:20.1Z: exception c0000409 in zenha.exe+000000000355fc35 (version 0.1.17.0)"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn clean_hides_the_profile_path_in_any_case() {
        assert_eq!(
            replace_ignore_case(
                r"open C:\Users\Ana\x and c:\users\ana\y",
                r"C:\Users\Ana",
                "%USERPROFILE%"
            ),
            r"open %USERPROFILE%\x and %USERPROFILE%\y"
        );
    }

    #[test]
    fn layer_queues_only_the_apps_warnings_and_errors() {
        use tracing_subscriber::prelude::*;
        let (tx, rx) = sync_channel(8);
        let subscriber = tracing_subscriber::registry().with(ReportLayer::new(tx));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "zenha_core::audio", err = "boom", what = "microphone", "audio device failed");
            tracing::info!(target: "zenha_core::audio", "audio started");
            tracing::error!(target: "livekit::rtc", "noise");
            tracing::error!(target: "zenha::app", backtrace = "0: main", "panic");
        });
        let got: Vec<(ReportLevel, String)> = rx.try_iter().map(|e| (e.level, e.message)).collect();
        assert_eq!(
            got,
            vec![
                (
                    ReportLevel::Warn,
                    "audio device failed err=boom what=microphone".to_owned()
                ),
                (ReportLevel::Error, "panic".to_owned()),
            ]
        );
    }

    #[test]
    fn webrtc_audio_messages_go_along_with_the_next_report() {
        use tracing_subscriber::prelude::*;
        let (tx, rx) = sync_channel(16);
        let subscriber = tracing_subscriber::registry().with(ReportLayer::new(tx));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                log.target = "libwebrtc",
                "(audio_device_core_win.cc:1): format not supported"
            );
            tracing::error!(
                log.target = "libwebrtc",
                "(audio_device_core_win.cc:2): Attempt to set Windows AEC with recording already initialized"
            );
            tracing::error!(
                log.target = "libwebrtc",
                "(peer_connection.cc:3): unrelated"
            );
        });
        assert!(rx.try_recv().is_err());
        let (tx, rx) = sync_channel(16);
        let layer = ReportLayer::new(tx);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                log.target = "libwebrtc",
                "(audio_device_core_win.cc:1): format not supported"
            );
            tracing::warn!(target: "zenha::core_session::translate", "could not select the input device");
            tracing::error!(
                log.target = "libwebrtc",
                "(audio_device_core_win.cc:4): IAudioClient::Initialize failed"
            );
        });
        let got: Vec<(String, String)> = rx.try_iter().map(|e| (e.target, e.message)).collect();
        assert_eq!(
            got,
            vec![
                (
                    "libwebrtc".to_owned(),
                    "(audio_device_core_win.cc:1): format not supported".to_owned()
                ),
                (
                    "zenha::core_session::translate".to_owned(),
                    "could not select the input device".to_owned()
                ),
                (
                    "libwebrtc".to_owned(),
                    "(audio_device_core_win.cc:4): IAudioClient::Initialize failed".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn run_started_reads_the_log_name() {
        let started = run_started(Path::new("zenha-1795221307-27392.log")).unwrap();
        assert_eq!(
            started.duration_since(UNIX_EPOCH).unwrap(),
            Duration::from_secs(1_795_221_307)
        );
        assert!(run_started(Path::new("other.log")).is_none());
    }
}
