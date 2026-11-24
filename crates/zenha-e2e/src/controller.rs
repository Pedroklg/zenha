//! Controlling a bot as a child process: commands on stdin, event history from stdout.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use zenha_core::VideoCodec;

use crate::protocol::{BotCommand, BotEvent, Stamped, decode_event};
use crate::scenario::tail_lines;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Process whose sound the bots share with their screen; 0 leaves the product default.
pub static SHARE_AUDIO_PID: AtomicU32 = AtomicU32::new(0);

/// Child process playing the side tone; killed when dropped.
pub struct SideTone {
    child: Child,
}

impl SideTone {
    pub fn start(exe: &Path, hz: f64, amplitude: f64) -> anyhow::Result<Self> {
        let mut child = Command::new(exe)
            .arg("side-tone")
            .args([
                "--hz",
                &hz.to_string(),
                "--amplitude",
                &amplitude.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("starting the side tone")?;
        let stdout = child.stdout.take().context("side tone without stdout")?;
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .context("reading the side tone status")?;
        if !line.starts_with("ready") {
            let mut reason = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut stderr, &mut reason);
            }
            let _ = child.kill();
            let _ = child.wait();
            bail!("the side tone did not start: {}", reason.trim());
        }
        SHARE_AUDIO_PID.store(child.id(), Ordering::SeqCst);
        Ok(Self { child })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for SideTone {
    fn drop(&mut self) {
        SHARE_AUDIO_PID.store(0, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Clone)]
pub struct BotSpec {
    pub name: String,
    pub token_url: String,
    pub invite: String,
    pub tone: Option<(f64, f64)>,
    pub test_pattern: bool,
    pub test_pattern_size: Option<(u32, u32)>,
    pub video_codec: VideoCodec,
    /// Replaces the enhanced microphone with the bot's own sound times this gain.
    pub echo_test: Option<f32>,
    /// Off keeps the older scenarios on the SFU path they were written for.
    pub p2p: bool,
}

#[derive(Default)]
struct Shared {
    events: Mutex<Vec<Stamped>>,
    changed: Condvar,
    closed: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct BotProcess {
    name: String,
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    shared: Arc<Shared>,
    log_path: PathBuf,
}

impl BotProcess {
    /// Starts the bot and waits for its `ready` event.
    pub fn spawn(
        exe: &Path,
        spec: &BotSpec,
        work_dir: &Path,
        log_path: &Path,
    ) -> anyhow::Result<Arc<Self>> {
        let log = File::create(log_path)
            .with_context(|| format!("creating the bot log {}", log_path.display()))?;
        let mut command = Command::new(exe);
        command
            .arg("bot")
            .args(["--name", &spec.name])
            .args(["--token-url", &spec.token_url])
            .args(["--invite", &spec.invite])
            .current_dir(work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        if let Some((hz, amplitude)) = spec.tone {
            command
                .args(["--tone", &hz.to_string()])
                .args(["--amplitude", &amplitude.to_string()]);
        }
        if spec.test_pattern {
            command.arg("--test-pattern");
        }
        if let Some((width, height)) = spec.test_pattern_size {
            command.args(["--test-pattern-size", &format!("{width}x{height}")]);
        }
        command.args(["--video-codec", spec.video_codec.as_str()]);
        command.args(["--p2p", if spec.p2p { "on" } else { "off" }]);
        if cfg!(not(windows)) {
            command.arg("--allow-non-windows");
        }
        command.env("ZENHA_AUDIO_TAP", "1");
        if let Some(gain) = spec.echo_test {
            command.env("ZENHA_ECHO_TEST", gain.to_string());
        }
        // A bot that shares its screen sends the sound of the side-tone process, nothing else.
        let share_audio = SHARE_AUDIO_PID.load(Ordering::SeqCst);
        if share_audio != 0 {
            command.env("ZENHA_SHARE_AUDIO_PID", share_audio.to_string());
        }
        // Only matters for a bot switched to the enhanced microphone.
        command.env(
            "ZENHA_ENHANCED_TEST_TONE",
            format!(
                "{},{}",
                crate::detect::TONE_HZ,
                crate::detect::TONE_AMPLITUDE
            ),
        );
        if std::env::var_os("RUST_LOG").is_none() {
            command.env("RUST_LOG", "info,libwebrtc=debug");
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("starting the bot {}", spec.name))?;
        let stdin = child.stdin.take().context("the bot has no stdin")?;
        let stdout = child.stdout.take().context("the bot has no stdout")?;
        let shared = Arc::new(Shared::default());
        let reader_shared = shared.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(event) = decode_event(&line) {
                    lock(&reader_shared.events).push(event);
                    reader_shared.changed.notify_all();
                }
            }
            reader_shared.closed.store(true, Ordering::SeqCst);
            reader_shared.changed.notify_all();
        });
        let bot = Arc::new(Self {
            name: spec.name.clone(),
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            shared,
            log_path: log_path.to_owned(),
        });
        bot.wait(0, READY_TIMEOUT, "the ready event", |event| {
            matches!(event, BotEvent::Ready { .. })
        })?;
        Ok(bot)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn pid(&self) -> Option<u32> {
        lock(&self.child).as_ref().map(Child::id)
    }

    /// Position in the event history; events from here on are "new".
    pub fn mark(&self) -> usize {
        lock(&self.shared.events).len()
    }

    /// Sends a command and returns the mark taken just before it.
    pub fn send(&self, command: &BotCommand) -> anyhow::Result<usize> {
        let mark = self.mark();
        let line = serde_json::to_string(command).context("encoding a command")?;
        let mut guard = lock(&self.stdin);
        let stdin = guard
            .as_mut()
            .ok_or_else(|| anyhow!("bot {} already closed its input", self.name))?;
        writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .with_context(|| format!("sending a command to bot {}", self.name))?;
        Ok(mark)
    }

    pub fn find(&self, since: usize, pred: impl Fn(&BotEvent) -> bool) -> Option<(usize, Stamped)> {
        lock(&self.shared.events)
            .iter()
            .enumerate()
            .skip(since)
            .find(|(_, stamped)| pred(&stamped.event))
            .map(|(index, stamped)| (index, stamped.clone()))
    }

    pub fn events_since(&self, since: usize) -> Vec<Stamped> {
        lock(&self.shared.events)
            .iter()
            .skip(since)
            .cloned()
            .collect()
    }

    /// Waits for an event after `since` that satisfies `pred`; returns its index and the event.
    /// A `failed` or `command_failed` event that `pred` does not accept ends the wait with an error.
    pub fn wait(
        &self,
        since: usize,
        timeout: Duration,
        what: &str,
        pred: impl Fn(&BotEvent) -> bool,
    ) -> anyhow::Result<(usize, Stamped)> {
        let deadline = Instant::now() + timeout;
        let mut guard = lock(&self.shared.events);
        loop {
            if let Some((index, stamped)) = guard
                .iter()
                .enumerate()
                .skip(since)
                .find(|(_, stamped)| pred(&stamped.event))
            {
                return Ok((index, stamped.clone()));
            }
            if let Some(failure) = guard.iter().skip(since).find_map(|s| match &s.event {
                BotEvent::CommandFailed { error } | BotEvent::Failed { error } => {
                    Some(format!("{}: {}", error.kind, error.message))
                }
                _ => None,
            }) {
                bail!(
                    "bot {} reported a failure ({failure}) while waiting for {what}",
                    self.name
                );
            }
            if self.closed() {
                bail!("bot {} exited while waiting for {what}", self.name);
            }
            let now = Instant::now();
            if now >= deadline {
                let seen = guard.len().saturating_sub(since);
                bail!(
                    "timed out after {:.0}s waiting for {what} on bot {} ({seen} events since the mark)",
                    timeout.as_secs_f64(),
                    self.name
                );
            }
            guard = self
                .shared
                .changed
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    pub fn log_tail(&self, count: usize) -> Vec<String> {
        let text = fs::read(&self.log_path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        tail_lines(&text, count)
    }

    /// Asks the bot to leave and exit, then kills it if it does not.
    pub fn quit(&self) {
        let _ = self.send(&BotCommand::Quit);
        *lock(&self.stdin) = None;
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            let mut guard = lock(&self.child);
            let Some(child) = guard.as_mut() else { return };
            match child.try_wait() {
                Ok(Some(_)) => {
                    guard.take();
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    drop(guard);
                    thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    guard.take();
                    return;
                }
            }
        }
    }
}

impl Drop for BotProcess {
    fn drop(&mut self) {
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
