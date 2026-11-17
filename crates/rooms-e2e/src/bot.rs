//! Participante sem janela: lê comandos JSON do stdin e escreve eventos JSON no stdout.

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use rooms_core::{Command, Event, FrameSlot, SessionHandle};

use crate::frames::{frame_stats, pacing};
use crate::protocol::{BotCommand, BotEvent, encode_event, parse_command};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const LEAVE_WAIT: Duration = Duration::from_secs(5);
const MAX_MEASURE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotInfo {
    pub name: String,
    pub microphone: String,
    pub test_pattern: bool,
}

enum Input {
    Line(String),
    Eof,
}

type Watched = Option<(String, FrameSlot)>;

fn spawn_reader<R: BufRead + Send + 'static>(input: R) -> mpsc::Receiver<Input> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            if tx.send(Input::Line(line)).is_err() {
                return;
            }
        }
        let _ = tx.send(Input::Eof);
    });
    rx
}

fn spawn_measure(watched: Watched, duration: Duration, out: Sender<BotEvent>) {
    thread::spawn(move || {
        let Some((identity, frames)) = watched else {
            let _ = out.send(BotEvent::FrameMeasurementFailed {
                reason: "not watching any screen share".to_owned(),
            });
            return;
        };
        let first = frames.generation();
        let started = Instant::now();
        let mut arrivals = Vec::new();
        let mut generation = first;
        while started.elapsed() < duration.min(MAX_MEASURE) {
            let now = frames.generation();
            if now != generation {
                generation = now;
                arrivals.push(started.elapsed().as_secs_f64());
            }
            thread::sleep(Duration::from_millis(1));
        }
        let seconds = started.elapsed().as_secs_f64();
        let received = frames.generation().saturating_sub(first);
        let (gap_p95_ms, gap_max_ms, stutters) = pacing(&arrivals);
        let mut seen = 0;
        let latest = frames
            .take_if_newer(&mut seen)
            .and_then(|frame| frame_stats(&frame));
        let event = match latest {
            Some(stats) => BotEvent::FrameMeasurement {
                identity,
                frames: received,
                seconds,
                fps: received as f64 / seconds.max(f64::EPSILON),
                width: stats.width,
                height: stats.height,
                mean_rgb: stats.mean_rgb,
                luma_stddev: stats.luma_stddev,
                gap_p95_ms,
                gap_max_ms,
                stutters,
            },
            None => BotEvent::FrameMeasurementFailed {
                reason: "no valid frame has been received".to_owned(),
            },
        };
        let _ = out.send(event);
    });
}

struct Emitter<W: Write> {
    output: W,
    started: Instant,
}

impl<W: Write> Emitter<W> {
    fn emit(&mut self, event: &BotEvent) -> io::Result<()> {
        let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        writeln!(self.output, "{}", encode_event(elapsed, event))?;
        self.output.flush()
    }
}

/// Runs until `quit`, end of input or a closed output. The handle is dropped on return.
pub fn run_bot<R, W>(
    mut handle: SessionHandle,
    info: &BotInfo,
    input: R,
    output: W,
) -> anyhow::Result<()>
where
    R: BufRead + Send + 'static,
    W: Write,
{
    let lines = spawn_reader(input);
    let (measure_tx, measure_rx) = mpsc::channel::<BotEvent>();
    let mut emitter = Emitter {
        output,
        started: Instant::now(),
    };
    emitter
        .emit(&BotEvent::Ready {
            pid: std::process::id(),
            name: info.name.clone(),
            microphone: info.microphone.clone(),
            test_pattern: info.test_pattern,
        })
        .context("writing the ready event")?;

    let mut watched: Watched = None;
    let mut in_room = false;
    'main: loop {
        while let Some(event) = handle.try_recv() {
            track(&event, &mut watched, &mut in_room);
            if let Some(logged) = BotEvent::from_core(&event)
                && emitter.emit(&logged).is_err()
            {
                break 'main;
            }
        }
        while let Ok(event) = measure_rx.try_recv() {
            if emitter.emit(&event).is_err() {
                break 'main;
            }
        }
        match lines.recv_timeout(POLL_INTERVAL) {
            Ok(Input::Line(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                match parse_command(&line) {
                    Ok(BotCommand::Quit) => break,
                    Ok(BotCommand::MeasureFrames { duration_ms }) => {
                        spawn_measure(
                            watched.clone(),
                            Duration::from_millis(duration_ms),
                            measure_tx.clone(),
                        );
                    }
                    Ok(command) => {
                        if let Some(core) = command.to_core() {
                            handle.send(core);
                        }
                    }
                    Err(reason) => {
                        let rejected = BotEvent::BadCommand { line, reason };
                        if emitter.emit(&rejected).is_err() {
                            break;
                        }
                    }
                }
            }
            Ok(Input::Eof) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }

    if in_room {
        handle.send(Command::Leave);
        let deadline = Instant::now() + LEAVE_WAIT;
        while in_room && Instant::now() < deadline {
            match handle.try_recv() {
                Some(event) => {
                    track(&event, &mut watched, &mut in_room);
                    if let Some(logged) = BotEvent::from_core(&event) {
                        let _ = emitter.emit(&logged);
                    }
                }
                None => thread::sleep(POLL_INTERVAL),
            }
        }
    }
    let _ = emitter.emit(&BotEvent::Bye);
    Ok(())
}

fn track(event: &Event, watched: &mut Watched, in_room: &mut bool) {
    match event {
        Event::WatchingStarted { identity, frames } => {
            *watched = Some((identity.clone(), frames.clone()));
        }
        Event::WatchingStopped => *watched = None,
        Event::Joined { .. } => *in_room = true,
        Event::Left | Event::RemovedFromRoom { .. } | Event::Failed(_) => {
            *in_room = false;
            *watched = None;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Read};
    use std::sync::mpsc::Receiver;
    use std::sync::{Arc, Mutex};

    use rooms_core::testing::{FakeBackend, FakeTokenClient};
    use rooms_core::{BackendEvent, Participant, SessionConfig, VideoFrame};

    use super::*;
    use crate::protocol::{Stamped, decode_event};

    struct ChannelReader {
        rx: Receiver<Vec<u8>>,
        buffer: Vec<u8>,
        position: usize,
    }

    impl Read for ChannelReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.position >= self.buffer.len() {
                match self.rx.recv() {
                    Ok(chunk) => {
                        self.buffer = chunk;
                        self.position = 0;
                    }
                    Err(_) => return Ok(0),
                }
            }
            let count = out.len().min(self.buffer.len() - self.position);
            out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
            self.position += count;
            Ok(count)
        }
    }

    #[derive(Clone, Default)]
    struct SharedOutput(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedOutput {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SharedOutput {
        fn events(&self) -> Vec<Stamped> {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .lines()
                .filter_map(decode_event)
                .collect()
        }
    }

    struct Rig {
        commands: Sender<Vec<u8>>,
        output: SharedOutput,
        backend: FakeBackend,
        thread: Option<thread::JoinHandle<anyhow::Result<()>>>,
    }

    impl Rig {
        fn start() -> Self {
            let backend = FakeBackend::new();
            backend.set_local_identity("bot");
            backend.set_participants(vec![Participant {
                identity: "bot".into(),
                name: "bot".into(),
                muted: false,
                speaking: false,
                avatar_version: None,
            }]);
            let config = SessionConfig {
                token_server_url: "http://unused.invalid".into(),
                client_id: "bot".into(),
                display_name: "bot".into(),
                invite_code: "code".into(),
                avatar_version: None,
                avatar_cache_dir: None,
                room_keys: Default::default(),
            };
            let handle =
                SessionHandle::spawn(config, backend.clone(), FakeTokenClient::new()).unwrap();
            let (commands, rx) = mpsc::channel();
            let reader = BufReader::new(ChannelReader {
                rx,
                buffer: Vec::new(),
                position: 0,
            });
            let output = SharedOutput::default();
            let sink = output.clone();
            let info = BotInfo {
                name: "bot".into(),
                microphone: "device".into(),
                test_pattern: true,
            };
            let thread = thread::spawn(move || run_bot(handle, &info, reader, sink));
            Self {
                commands,
                output,
                backend,
                thread: Some(thread),
            }
        }

        fn send(&self, line: &str) {
            self.commands
                .send(format!("{line}\n").into_bytes())
                .unwrap();
        }

        fn wait_for(&self, what: &str, pred: impl Fn(&BotEvent) -> bool) -> BotEvent {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(found) = self.output.events().into_iter().find(|s| pred(&s.event)) {
                    return found.event;
                }
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                thread::sleep(Duration::from_millis(5));
            }
        }

        fn finish(mut self) -> Vec<BotEvent> {
            self.send(r#"{"cmd":"quit"}"#);
            self.thread.take().unwrap().join().unwrap().unwrap();
            self.output.events().into_iter().map(|s| s.event).collect()
        }
    }

    #[test]
    fn bot_joins_reports_events_and_quits_cleanly() {
        let rig = Rig::start();
        rig.wait_for("ready", |e| matches!(e, BotEvent::Ready { .. }));
        rig.send(r#"{"cmd":"join","room":"sala"}"#);
        let joined = rig.wait_for("joined", |e| matches!(e, BotEvent::Joined { .. }));
        assert!(matches!(joined, BotEvent::Joined { ref room, .. } if room == "sala"));

        rig.backend
            .emit(BackendEvent::ParticipantJoined(Participant {
                identity: "other".into(),
                name: "Other".into(),
                muted: false,
                speaking: false,
                avatar_version: None,
            }));
        rig.wait_for("participant_joined", |e| {
            matches!(e, BotEvent::ParticipantJoined { participant } if participant.identity == "other")
        });
        rig.send(r#"{"cmd":"set_muted","muted":true}"#);
        rig.send(r#"{"cmd":"send-chat","text":"oi"}"#);
        rig.wait_for(
            "own chat echo",
            |e| matches!(e, BotEvent::ChatReceived { text, .. } if text == "oi"),
        );
        rig.send("garbage");
        rig.wait_for("bad command", |e| matches!(e, BotEvent::BadCommand { .. }));

        let events = rig.finish();
        let names: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, BotEvent::Left | BotEvent::Bye))
            .collect();
        assert_eq!(names, vec![&BotEvent::Left, &BotEvent::Bye]);
    }

    #[test]
    fn measure_frames_without_watching_fails_and_with_watching_counts_frames() {
        let rig = Rig::start();
        rig.send(r#"{"cmd":"join","room":"sala"}"#);
        rig.wait_for("joined", |e| matches!(e, BotEvent::Joined { .. }));
        rig.send(r#"{"cmd":"measure_frames","duration_ms":10}"#);
        rig.wait_for("failure", |e| {
            matches!(e, BotEvent::FrameMeasurementFailed { .. })
        });

        rig.send(r#"{"cmd":"watch","identity":"other"}"#);
        rig.wait_for("watching", |e| {
            matches!(e, BotEvent::WatchingStarted { .. })
        });
        rig.send(r#"{"cmd":"measure_frames","duration_ms":300}"#);
        let backend = rig.backend.clone();
        let pusher = thread::spawn(move || {
            for _ in 0..10 {
                backend.push_frame(VideoFrame {
                    width: 2,
                    height: 2,
                    rgba: [200, 100, 50, 255].repeat(4),
                });
                thread::sleep(Duration::from_millis(20));
            }
        });
        let measured = rig.wait_for("measurement", |e| {
            matches!(e, BotEvent::FrameMeasurement { .. })
        });
        pusher.join().unwrap();
        match measured {
            BotEvent::FrameMeasurement {
                frames,
                width,
                height,
                mean_rgb,
                ..
            } => {
                assert!((5..=10).contains(&frames), "{frames}");
                assert_eq!((width, height), (2, 2));
                assert_eq!(mean_rgb, [200.0, 100.0, 50.0]);
            }
            other => panic!("unexpected {other:?}"),
        }
        rig.finish();
    }

    #[test]
    fn end_of_input_stops_the_bot() {
        let rig = Rig::start();
        let Rig {
            commands,
            output,
            thread,
            ..
        } = rig;
        drop(commands);
        thread.unwrap().join().unwrap().unwrap();
        assert!(matches!(
            output.events().last().map(|s| &s.event),
            Some(BotEvent::Bye)
        ));
    }
}
