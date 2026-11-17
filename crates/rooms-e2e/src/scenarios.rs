//! Os dez cenários e as ferramentas que eles compartilham.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use rooms_core::{SharePreset, TEST_PATTERN_SIZE, fit_within};
use rooms_protocol::MAX_FILE_BYTES;

use crate::controller::{BotProcess, BotSpec, SideTone};
use crate::detect::{
    MIN_TONE_RMS, TONE_AMPLITUDE, TONE_HZ, baseline_is_noisy, tone_absent, tone_present,
};
use crate::dsp::ToneMeasurement;
use crate::frames::{MIN_LUMA_STDDEV, MIN_PLAUSIBLE_FPS, colour_matches, expected_pattern_mean};
use crate::probe::{AudioProbe, RenderDevice, match_sdk_output};
use crate::proc::{http_request, require_status};
use crate::protocol::{BotCommand, BotEvent, DeviceInfo, NoiseKind};
use crate::report::Environment;
use crate::scenario::{BotLog, Recorder, ScenarioResult};

const JOIN_TIMEOUT: Duration = Duration::from_secs(25);
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);
const BASELINE_WINDOW: Duration = Duration::from_millis(1000);
const POLL_WINDOW: Duration = Duration::from_millis(500);
const TONE_WITHIN: Duration = Duration::from_secs(13);
const ABSENT_WITHIN: Duration = Duration::from_secs(8);
const FRAME_WINDOW_MS: u64 = 3000;
const LOG_TAIL_LINES: usize = 30;
const TONE_BOT: &str = "tone";
const LISTENER_BOT: &str = "listener";
const SMALL_FILE_BYTES: u64 = 200 * 1024;
const BIG_FILE_TIMEOUT: Duration = Duration::from_secs(180);
const NO_TRANSFER_WINDOW: Duration = Duration::from_secs(2);
const FILE_BLOCK: usize = 64 * 1024;

pub struct Setup {
    pub exe: PathBuf,
    pub work_dir: PathBuf,
    pub out_dir: PathBuf,
    pub token_addr: SocketAddr,
    pub invite: String,
}

impl Setup {
    pub fn token_url(&self) -> String {
        format!("http://{}", self.token_addr)
    }
}

struct ListenerDevices {
    inputs: Vec<DeviceInfo>,
    outputs: Vec<DeviceInfo>,
    selected_output: Option<String>,
}

pub struct ScenarioDef {
    pub name: &'static str,
    pub title: &'static str,
    pub needs_media: bool,
    pub run: fn(&mut Ctx<'_>) -> anyhow::Result<()>,
}

pub const ALL: &[ScenarioDef] = &[
    ScenarioDef {
        name: "token",
        title: "Convite correto entra, convite errado é recusado",
        needs_media: false,
        run: token,
    },
    ScenarioDef {
        name: "presence",
        title: "Entrada e saída de participantes aparecem para os outros",
        needs_media: true,
        run: presence,
    },
    ScenarioDef {
        name: "playout-listener-first",
        title: "Ouvinte entra primeiro, o tom chega à saída de áudio",
        needs_media: true,
        run: playout_listener_first,
    },
    ScenarioDef {
        name: "playout-tone-first",
        title: "Tom entra primeiro, o tom chega à saída de áudio",
        needs_media: true,
        run: playout_tone_first,
    },
    ScenarioDef {
        name: "playout-explicit-device",
        title: "Como o anterior, com a saída padrão escolhida explicitamente",
        needs_media: true,
        run: playout_explicit_device,
    },
    ScenarioDef {
        name: "enhanced-mic",
        title: "Microfone reforçado publica só o próprio áudio e troca de modo com a sala aberta",
        needs_media: true,
        run: enhanced_mic,
    },
    ScenarioDef {
        name: "maximum-mic",
        title: "Microfone no modo Máxima (DeepFilterNet) publica só o próprio áudio",
        needs_media: true,
        run: maximum_mic,
    },
    ScenarioDef {
        name: "mute",
        title: "Mute de quem fala some e volta na saída de áudio",
        needs_media: true,
        run: mute,
    },
    ScenarioDef {
        name: "deafen",
        title: "Deafen de quem ouve silencia e restaura a saída de áudio",
        needs_media: true,
        run: deafen,
    },
    ScenarioDef {
        name: "chat",
        title: "Mensagem de chat chega com o remetente certo",
        needs_media: true,
        run: chat,
    },
    ScenarioDef {
        name: "files",
        title: "Arquivo enviado pelo chat chega íntegro, e o limite de 50 MB é respeitado",
        needs_media: true,
        run: files,
    },
    ScenarioDef {
        name: "share",
        title: "Transmissão de tela chega em quadros e o áudio continua",
        needs_media: true,
        run: share,
    },
    ScenarioDef {
        name: "share-audio",
        title: "Som da transmissão chega a quem assiste e obedece ao volume próprio",
        needs_media: true,
        run: share_audio,
    },
    ScenarioDef {
        name: "participant-volume",
        title: "Volume por pessoa abaixa, zera e passa de 100%",
        needs_media: true,
        run: participant_volume,
    },
    ScenarioDef {
        name: "room-switch",
        title: "Troca de sala e volta com presença e áudio",
        needs_media: true,
        run: room_switch,
    },
    ScenarioDef {
        name: "cue",
        title: "Som de confirmação de mute e deafen toca na saída escolhida",
        needs_media: true,
        run: cue,
    },
];

pub fn room_name(scenario: &str) -> String {
    format!("e2e-{scenario}")
}

pub fn second_room_name(scenario: &str) -> String {
    format!("e2e-{scenario}-b")
}

pub fn all_room_names() -> Vec<String> {
    ALL.iter()
        .flat_map(|def| [room_name(def.name), second_room_name(def.name)])
        .collect()
}

pub struct Ctx<'a> {
    pub rec: Recorder,
    setup: &'a Setup,
    probe: &'a dyn AudioProbe,
    env: &'a mut Environment,
    bots: Vec<Arc<BotProcess>>,
    name: &'static str,
}

impl<'a> Ctx<'a> {
    pub fn new(
        def: &ScenarioDef,
        setup: &'a Setup,
        probe: &'a dyn AudioProbe,
        env: &'a mut Environment,
    ) -> Self {
        Self {
            rec: Recorder::new(def.name, def.title),
            setup,
            probe,
            env,
            bots: Vec::new(),
            name: def.name,
        }
    }

    pub fn finish(self, outcome: anyhow::Result<()>) -> ScenarioResult {
        let Self { rec, bots, .. } = self;
        for bot in &bots {
            bot.quit();
        }
        let logs = bots
            .iter()
            .map(|bot| BotLog {
                bot: bot.name().to_owned(),
                tail: bot.log_tail(LOG_TAIL_LINES),
            })
            .collect();
        rec.finish(outcome, logs)
    }

    fn room(&self) -> String {
        room_name(self.name)
    }

    fn room_b(&self) -> String {
        second_room_name(self.name)
    }

    fn spawn(
        &mut self,
        name: &str,
        tone: bool,
        pattern: bool,
        invite: &str,
    ) -> anyhow::Result<Arc<BotProcess>> {
        let spec = BotSpec {
            name: name.to_owned(),
            token_url: self.setup.token_url(),
            invite: invite.to_owned(),
            tone: tone.then_some((TONE_HZ, TONE_AMPLITUDE)),
            test_pattern: pattern,
        };
        let log = self
            .setup
            .out_dir
            .join(format!("bot-{}-{name}.log", self.name));
        let bot = BotProcess::spawn(&self.setup.exe, &spec, &self.setup.work_dir, &log)?;
        self.bots.push(bot.clone());
        self.rec.step(format!(
            "bot {name} started ({})",
            if tone { "tone" } else { "real audio device" }
        ));
        Ok(bot)
    }

    fn spawn_tone(&mut self, name: &str, pattern: bool) -> anyhow::Result<Arc<BotProcess>> {
        let invite = self.setup.invite.clone();
        self.spawn(name, true, pattern, &invite)
    }

    fn spawn_listener(&mut self, name: &str) -> anyhow::Result<Arc<BotProcess>> {
        let invite = self.setup.invite.clone();
        let bot = self.spawn(name, false, false, &invite)?;
        let devices = self.refresh_devices(&bot)?;
        if self.env.listener_outputs.is_empty() {
            self.env.listener_inputs = devices.inputs;
            self.env.listener_outputs = devices.outputs;
            self.env.listener_selected_output = devices.selected_output;
        }
        if self.probe.pinned() {
            let device = self.probe.default_render_device()?;
            self.select_output(&bot, &device)?;
        }
        Ok(bot)
    }

    fn select_output(&mut self, bot: &BotProcess, device: &RenderDevice) -> anyhow::Result<()> {
        let outputs = self.refresh_devices(bot)?.outputs;
        let Some(chosen) = match_sdk_output(device, &outputs) else {
            bail!(
                "no client output device matches `{}` (`{}`); the client lists: {}",
                device.name,
                device.id,
                outputs
                    .iter()
                    .map(|d| format!("{} `{}`", d.name, d.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
        let chosen_id = chosen.id.clone();
        self.rec.text("selected_output_name", chosen.name.clone());
        self.rec.text("selected_output_id", chosen_id.clone());
        let select = bot.send(&BotCommand::SetOutputDevice {
            id: Some(chosen_id.clone()),
        })?;
        bot.wait(select, EVENT_TIMEOUT, "the output selection", move |e| {
            matches!(e, BotEvent::AudioDevicesUpdated { selected_output, .. } if selected_output.as_deref() == Some(chosen_id.as_str()))
        })?;
        Ok(())
    }

    fn refresh_devices(&mut self, bot: &BotProcess) -> anyhow::Result<ListenerDevices> {
        let mark = bot.send(&BotCommand::RefreshDevices)?;
        let (_, found) = bot.wait(mark, EVENT_TIMEOUT, "the audio device list", |e| {
            matches!(e, BotEvent::AudioDevicesUpdated { .. })
        })?;
        let BotEvent::AudioDevicesUpdated {
            inputs,
            outputs,
            selected_output,
            ..
        } = found.event
        else {
            bail!("unexpected event while listing devices");
        };
        self.rec
            .number("listener_output_devices", outputs.len() as f64, "");
        Ok(ListenerDevices {
            inputs,
            outputs,
            selected_output,
        })
    }

    fn join(&mut self, bot: &BotProcess, room: &str) -> anyhow::Result<Instant> {
        let started = Instant::now();
        let mark = bot.send(&BotCommand::Join {
            room: room.to_owned(),
        })?;
        bot.wait(
            mark,
            JOIN_TIMEOUT,
            &format!("{} to join {room}", bot.name()),
            |e| matches!(e, BotEvent::Joined { room: joined, .. } if joined == room),
        )?;
        let joined_at = Instant::now();
        self.rec.step(format!(
            "{} joined {room} in {} ms",
            bot.name(),
            started.elapsed().as_millis()
        ));
        Ok(joined_at)
    }

    fn measure_ms(&mut self, name: &str, since: Instant) {
        self.rec
            .number(name, since.elapsed().as_secs_f64() * 1000.0, "ms");
    }

    fn require_probe(&self) -> anyhow::Result<()> {
        self.probe
            .render_devices()
            .map(|_| ())
            .context("the audio probe is not working")
    }

    fn baseline(&mut self) -> anyhow::Result<ToneMeasurement> {
        self.require_probe()?;
        let m = self.probe.measure_tone(TONE_HZ, BASELINE_WINDOW)?;
        self.rec.number("baseline_total_rms", m.total_rms, "");
        self.rec.number("baseline_tone_rms", m.tone_rms, "");
        self.rec.number("baseline_coverage", m.coverage, "");
        if baseline_is_noisy(&m) {
            self.rec.step(format!(
                "warning: sound was already playing before the test (total_rms {:.4}), results are less reliable",
                m.total_rms
            ));
        }
        Ok(m)
    }

    fn wait_tone_present(
        &mut self,
        label: &str,
        baseline: &ToneMeasurement,
        since: Instant,
        within: Duration,
    ) -> anyhow::Result<ToneMeasurement> {
        let mut polls = 0u32;
        loop {
            let m = self.probe.measure_tone(TONE_HZ, POLL_WINDOW)?;
            polls += 1;
            let verdict = tone_present(baseline, &m);
            if verdict.ok {
                self.rec.number(
                    &format!("{label}_time_to_tone"),
                    since.elapsed().as_secs_f64(),
                    "s",
                );
                self.rec
                    .number(&format!("{label}_tone_rms"), m.tone_rms, "");
                self.rec
                    .number(&format!("{label}_tone_ratio"), m.tone_ratio, "");
                self.rec
                    .number(&format!("{label}_total_rms"), m.total_rms, "");
                self.rec.number(&format!("{label}_peak"), m.peak, "");
                self.rec
                    .step(format!("{label}: tone detected, {}", verdict.reason));
                return Ok(m);
            }
            if since.elapsed() >= within {
                self.rec
                    .number(&format!("{label}_polls"), f64::from(polls), "");
                self.rec
                    .number(&format!("{label}_last_tone_rms"), m.tone_rms, "");
                self.rec
                    .number(&format!("{label}_last_total_rms"), m.total_rms, "");
                self.rec
                    .number(&format!("{label}_last_coverage"), m.coverage, "");
                bail!(
                    "{label}: the {TONE_HZ} Hz tone did not reach the default output device within {:.0} s ({polls} measurements; last: {}; total_rms {:.5}, coverage {:.2})",
                    within.as_secs_f64(),
                    verdict.reason,
                    m.total_rms,
                    m.coverage
                );
            }
        }
    }

    fn wait_tone_absent(
        &mut self,
        label: &str,
        baseline: &ToneMeasurement,
        when_present: &ToneMeasurement,
        since: Instant,
        within: Duration,
    ) -> anyhow::Result<()> {
        loop {
            let m = self.probe.measure_tone(TONE_HZ, POLL_WINDOW)?;
            let verdict = tone_absent(baseline, when_present, &m);
            if verdict.ok {
                self.rec.number(
                    &format!("{label}_time_to_silence"),
                    since.elapsed().as_secs_f64(),
                    "s",
                );
                self.rec
                    .number(&format!("{label}_tone_rms"), m.tone_rms, "");
                self.rec
                    .step(format!("{label}: tone gone, {}", verdict.reason));
                return Ok(());
            }
            if since.elapsed() >= within {
                self.rec
                    .number(&format!("{label}_last_tone_rms"), m.tone_rms, "");
                bail!(
                    "{label}: the tone was still audible {:.0} s after the change ({})",
                    within.as_secs_f64(),
                    verdict.reason
                );
            }
        }
    }
}

fn token(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let addr = ctx.setup.token_addr;
    let request = |invite: &str| {
        serde_json::json!({"client_id": "alice", "display_name": "alice", "room": room_name("token"), "invite_code": invite})
            .to_string()
    };
    let valid = http_request(addr, "POST", "/token", Some(&request(&ctx.setup.invite)))?;
    require_status(&valid, 200, "token with the right invite")?;
    if !valid.body.contains("\"token\"") || !valid.body.contains("\"livekit_url\"") {
        bail!("the token response has no token or livekit_url field");
    }
    ctx.rec
        .number("http_status_right_invite", f64::from(valid.status), "");
    let wrong = http_request(addr, "POST", "/token", Some(&request("convite-errado")))?;
    require_status(&wrong, 401, "token with a wrong invite")?;
    ctx.rec
        .number("http_status_wrong_invite", f64::from(wrong.status), "");

    let room = ctx.room();
    let alice = ctx.spawn_tone("alice", false)?;
    ctx.join(&alice, &room)?;

    let mallory = ctx.spawn("mallory", true, false, "convite-errado")?;
    let mark = mallory.send(&BotCommand::Join { room })?;
    let (_, failed) = mallory.wait(mark, JOIN_TIMEOUT, "the invite rejection", |e| {
        matches!(e, BotEvent::Failed { .. })
    })?;
    match failed.event {
        BotEvent::Failed { error } if error.kind == "invite_rejected" => {
            ctx.rec.text("wrong_invite_error", error.kind);
            Ok(())
        }
        other => bail!("expected InviteRejected, got {other:?}"),
    }
}

fn presence(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let alice = ctx.spawn_tone("alice", false)?;
    let bob = ctx.spawn_tone("bob", false)?;
    ctx.join(&alice, &room)?;

    let alice_mark = alice.mark();
    let bob_joined_at = Instant::now();
    let (_, joined) = {
        let mark = bob.send(&BotCommand::Join { room: room.clone() })?;
        bob.wait(mark, JOIN_TIMEOUT, "bob to join", |e| {
            matches!(e, BotEvent::Joined { .. })
        })?
    };
    if let BotEvent::Joined { participants, .. } = &joined.event
        && !participants.iter().any(|p| p.identity == "alice")
    {
        bail!("bob's join snapshot does not list alice");
    }
    alice.wait(alice_mark, EVENT_TIMEOUT, "alice to see bob", |e| {
        matches!(e, BotEvent::ParticipantJoined { participant } if participant.identity == "bob")
    })?;
    ctx.measure_ms("alice_sees_bob_join", bob_joined_at);

    let alice_mark = alice.mark();
    let left_at = Instant::now();
    let bob_mark = bob.send(&BotCommand::Leave)?;
    bob.wait(bob_mark, EVENT_TIMEOUT, "bob's own left event", |e| {
        matches!(e, BotEvent::Left)
    })?;
    alice.wait(
        alice_mark,
        EVENT_TIMEOUT,
        "alice to see bob leave",
        |e| matches!(e, BotEvent::ParticipantLeft { identity } if identity == "bob"),
    )?;
    ctx.measure_ms("alice_sees_bob_leave", left_at);
    Ok(())
}

fn playout_listener_first(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    ctx.join(&listener, &room)?;
    let baseline = ctx.baseline()?;
    let tone = ctx.spawn_tone(TONE_BOT, false)?;
    let since = ctx.join(&tone, &room)?;
    ctx.wait_tone_present("playout", &baseline, since, TONE_WITHIN)?;
    Ok(())
}

fn playout_tone_first(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let baseline = ctx.baseline()?;
    let tone = ctx.spawn_tone(TONE_BOT, false)?;
    ctx.join(&tone, &room)?;
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    let since = ctx.join(&listener, &room)?;
    ctx.wait_tone_present("playout", &baseline, since, TONE_WITHIN)?;
    Ok(())
}

fn playout_explicit_device(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let baseline = ctx.baseline()?;
    let default = ctx.probe.default_render_device()?;
    ctx.rec.text("default_render_device", default.name.clone());
    let tone = ctx.spawn_tone(TONE_BOT, false)?;
    ctx.join(&tone, &room)?;

    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    ctx.select_output(&listener, &default)?;
    let since = ctx.join(&listener, &room)?;
    ctx.wait_tone_present("playout", &baseline, since, TONE_WITHIN)?;
    Ok(())
}

/// Tone and listener joined, tone confirmed, ready for the toggling scenarios.
struct AudibleRoom {
    baseline: ToneMeasurement,
    present: ToneMeasurement,
    tone: Arc<BotProcess>,
    listener: Arc<BotProcess>,
}

fn audible_room(ctx: &mut Ctx<'_>) -> anyhow::Result<AudibleRoom> {
    let room = ctx.room();
    let baseline = ctx.baseline()?;
    let tone = ctx.spawn_tone(TONE_BOT, false)?;
    ctx.join(&tone, &room)?;
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    let since = ctx.join(&listener, &room)?;
    let present = ctx.wait_tone_present("initial", &baseline, since, TONE_WITHIN)?;
    Ok(AudibleRoom {
        baseline,
        present,
        tone,
        listener,
    })
}

/// Level and dominant frequency a bot logged for the audio received from `identity`.
fn audio_taps(log: &[String], identity: &str, kind: &str) -> Vec<(f64, f64)> {
    let marker = format!("received audio tap identity={identity} kind=\"{kind}\" ");
    log.iter()
        .filter_map(|line| {
            let rest = line.split_once(marker.as_str())?.1;
            let field = |name: &str| -> Option<f64> {
                rest.split_whitespace()
                    .find_map(|part| part.strip_prefix(name))?
                    .parse()
                    .ok()
            };
            Some((field("rms=")?, field("hz=")?))
        })
        .collect()
}

/// The harness plays a tone on a third output device; the sharer sends the sound of the
/// harness process with its screen share; the listener plays that through the test device.
fn share_audio(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    const STREAM_HZ: f64 = 880.0;
    const STREAM_AMPLITUDE: f64 = 0.05;
    const SETTLE: Duration = Duration::from_millis(1500);
    const WINDOW: Duration = Duration::from_millis(1000);
    ctx.require_probe()?;
    let side_tone = SideTone::start(&ctx.setup.exe, STREAM_HZ, STREAM_AMPLITUDE)?;
    ctx.rec
        .number("side_tone_pid", f64::from(side_tone.pid()), "");
    let room = ctx.room();
    let sharer = ctx.spawn_tone(TONE_BOT, true)?;
    sharer.send(&BotCommand::SetMuted { muted: true })?;
    ctx.join(&sharer, &room)?;
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    listener.send(&BotCommand::SetStreamVolume { percent: 0 })?;
    ctx.join(&listener, &room)?;

    let listener_mark = listener.mark();
    let share_mark = sharer.send(&BotCommand::StartShare)?;
    sharer.wait(share_mark, JOIN_TIMEOUT, "the screen share to start", |e| {
        matches!(e, BotEvent::ScreenShareStarted)
    })?;
    listener.wait(
        listener_mark,
        JOIN_TIMEOUT,
        "the remote share announcement",
        |e| matches!(e, BotEvent::RemoteScreenShareStarted { identity } if identity == TONE_BOT),
    )?;
    let watch_mark = listener.send(&BotCommand::Watch {
        identity: TONE_BOT.to_owned(),
    })?;
    listener.wait(
        watch_mark,
        EVENT_TIMEOUT,
        "watching to start",
        |e| matches!(e, BotEvent::WatchingStarted { identity } if identity == TONE_BOT),
    )?;

    std::thread::sleep(Duration::from_secs(4));
    let taps = audio_taps(&listener.log_tail(usize::MAX), TONE_BOT, "stream");
    let Some((rms, hz)) = taps.last().copied() else {
        bail!("the listener received no stream audio");
    };
    ctx.rec.number("stream_tap_rms", rms, "");
    ctx.rec.number("stream_tap_hz", hz, "Hz");
    if (hz - STREAM_HZ).abs() > 25.0 || rms < 0.005 {
        bail!("the stream audio is not the side tone (rms {rms:.4}, {hz:.0} Hz)");
    }

    let silent = ctx.probe.measure_tone(STREAM_HZ, WINDOW)?;
    ctx.rec.number("volume_0_tone_rms", silent.tone_rms, "");
    if silent.tone_rms > MIN_TONE_RMS {
        bail!(
            "stream volume 0 still plays the stream (tone_rms {:.5})",
            silent.tone_rms
        );
    }

    listener.send(&BotCommand::SetStreamVolume { percent: 100 })?;
    std::thread::sleep(SETTLE);
    let full = ctx.probe.measure_tone(STREAM_HZ, WINDOW)?;
    ctx.rec.number("volume_100_tone_rms", full.tone_rms, "");
    ctx.rec.number("volume_100_tone_ratio", full.tone_ratio, "");
    if full.tone_rms < 0.005 || full.tone_ratio < 0.5 {
        bail!(
            "stream volume 100 does not play a clean stream (tone_rms {:.5}, tone_ratio {:.2})",
            full.tone_rms,
            full.tone_ratio
        );
    }

    listener.send(&BotCommand::SetStreamVolume { percent: 50 })?;
    std::thread::sleep(SETTLE);
    let half = ctx.probe.measure_tone(STREAM_HZ, WINDOW)?;
    ctx.rec.number("volume_50_tone_rms", half.tone_rms, "");
    let ratio = half.tone_rms / full.tone_rms;
    if !(0.15..=0.40).contains(&ratio) {
        bail!("stream volume 50 should play at a quarter of the level, got {ratio:.2}");
    }

    let stop_mark = listener.send(&BotCommand::StopWatching)?;
    listener.wait(stop_mark, EVENT_TIMEOUT, "watching to stop", |e| {
        matches!(e, BotEvent::WatchingStopped)
    })?;
    std::thread::sleep(SETTLE);
    let stopped = ctx.probe.measure_tone(STREAM_HZ, WINDOW)?;
    ctx.rec.number("after_stop_tone_rms", stopped.tone_rms, "");
    if stopped.tone_rms > MIN_TONE_RMS {
        bail!("the stream keeps playing after watching stopped");
    }
    Ok(())
}

/// The speaker's enhanced pipeline is fed the diagnostic tone after the noise suppressor, so
/// a clean tone at the far end proves the path works and the raw microphone is not mixed in.
fn enhanced_mic(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    own_capture_mic(ctx, NoiseKind::Enhanced).map(|_| ())
}

fn maximum_mic(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let speaker = own_capture_mic(ctx, NoiseKind::Maximum)?;
    if !speaker
        .log_tail(usize::MAX)
        .iter()
        .any(|line| line.contains("engine=\"deepfilternet\""))
    {
        bail!("the speaker never logged engine=\"deepfilternet\": it fell back to another engine");
    }
    Ok(())
}

fn own_capture_mic(ctx: &mut Ctx<'_>, mode: NoiseKind) -> anyhow::Result<Arc<BotProcess>> {
    const SPEAKER_BOT: &str = "speaker";
    let room = ctx.room();
    let baseline = ctx.baseline()?;
    let speaker = ctx.spawn_listener(SPEAKER_BOT)?;
    speaker.send(&BotCommand::SetNoiseSuppression { mode })?;
    speaker.send(&BotCommand::SetNoiseGate { db: Some(-40) })?;
    ctx.join(&speaker, &room)?;
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    let since = ctx.join(&listener, &room)?;
    let present = ctx.wait_tone_present("enhanced", &baseline, since, TONE_WITHIN)?;

    let since = Instant::now();
    speaker.send(&BotCommand::SetNoiseSuppression {
        mode: NoiseKind::Standard,
    })?;
    ctx.wait_tone_absent("standard", &baseline, &present, since, ABSENT_WITHIN)?;

    let since = Instant::now();
    speaker.send(&BotCommand::SetNoiseSuppression { mode })?;
    ctx.wait_tone_present("enhanced_again", &baseline, since, TONE_WITHIN)?;

    std::thread::sleep(Duration::from_secs(4));
    let taps = audio_taps(&listener.log_tail(usize::MAX), SPEAKER_BOT, "voice");
    let Some(recent) = taps.len().checked_sub(3).map(|start| &taps[start..]) else {
        bail!("the listener logged only {} audio taps", taps.len());
    };
    let expected_rms = TONE_AMPLITUDE / std::f64::consts::SQRT_2;
    for (index, (rms, hz)) in recent.iter().enumerate() {
        ctx.rec.number(&format!("tap_{index}_rms"), *rms, "");
        ctx.rec.number(&format!("tap_{index}_hz"), *hz, "Hz");
        if (hz - TONE_HZ).abs() > 25.0 || (rms / expected_rms - 1.0).abs() > 0.25 {
            bail!(
                "the received audio is not the clean tone (rms {rms:.4}, {hz:.0} Hz; expected rms {expected_rms:.4}, {TONE_HZ} Hz): the raw microphone is probably mixed in"
            );
        }
    }
    Ok(speaker)
}

fn mute(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let AudibleRoom {
        baseline,
        present,
        tone,
        listener,
    } = audible_room(ctx)?;

    for (muted, label) in [(true, "muted"), (false, "unmuted")] {
        let mark = listener.mark();
        let sent = Instant::now();
        tone.send(&BotCommand::SetMuted { muted })?;
        listener.wait(mark, EVENT_TIMEOUT, "the mute event", move |e| {
            matches!(e, BotEvent::ParticipantMuteChanged { identity, muted: m } if identity == TONE_BOT && *m == muted)
        })?;
        ctx.measure_ms(&format!("{label}_event_latency"), sent);
        if muted {
            ctx.wait_tone_absent(label, &baseline, &present, sent, ABSENT_WITHIN)?;
        } else {
            ctx.wait_tone_present(label, &baseline, sent, TONE_WITHIN)?;
        }
    }
    Ok(())
}

fn participant_volume(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    const SETTLE: Duration = Duration::from_millis(1500);
    const WINDOW: Duration = Duration::from_millis(1000);
    let AudibleRoom { listener, .. } = audible_room(ctx)?;

    let level = |ctx: &mut Ctx<'_>, percent: u16| -> anyhow::Result<f64> {
        listener.send(&BotCommand::SetParticipantVolume {
            identity: TONE_BOT.to_owned(),
            percent,
        })?;
        std::thread::sleep(SETTLE);
        let m = ctx.probe.measure_tone(TONE_HZ, WINDOW)?;
        ctx.rec
            .number(&format!("volume_{percent}_tone_rms"), m.tone_rms, "");
        Ok(m.tone_rms)
    };

    let full = level(ctx, 100)?;
    if full < MIN_TONE_RMS {
        bail!("volume 100 plays no tone (tone_rms {full:.5})");
    }
    let half = level(ctx, 50)? / full;
    if !(0.35..=0.70).contains(&half) {
        bail!("volume 50 should play at half the level, got {half:.2}");
    }
    let double = level(ctx, 200)? / full;
    if !(1.5..=2.6).contains(&double) {
        bail!("volume 200 should play at twice the level, got {double:.2}");
    }
    let silent = level(ctx, 0)?;
    if silent > MIN_TONE_RMS {
        bail!("volume 0 still plays the tone (tone_rms {silent:.5})");
    }
    Ok(())
}

fn deafen(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let AudibleRoom {
        baseline,
        present,
        listener,
        ..
    } = audible_room(ctx)?;

    let sent = Instant::now();
    listener.send(&BotCommand::SetDeafened { deafened: true })?;
    ctx.wait_tone_absent("deafened", &baseline, &present, sent, ABSENT_WITHIN)?;

    let sent = Instant::now();
    listener.send(&BotCommand::SetDeafened { deafened: false })?;
    ctx.wait_tone_present("undeafened", &baseline, sent, TONE_WITHIN)?;
    Ok(())
}

fn chat(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let alice = ctx.spawn_tone("alice", false)?;
    let bob = ctx.spawn_tone("bob", false)?;
    ctx.join(&alice, &room)?;
    ctx.join(&bob, &room)?;
    alice.wait(0, EVENT_TIMEOUT, "alice to see bob", |e| {
        matches!(e, BotEvent::ParticipantJoined { participant } if participant.identity == "bob")
            || matches!(e, BotEvent::Joined { participants, .. } if participants.iter().any(|p| p.identity == "bob"))
    })?;

    for (from, to, label) in [
        (&alice, &bob, "alice_to_bob"),
        (&bob, &alice, "bob_to_alice"),
    ] {
        let text = format!("e2e-{label}-{}", uuid::Uuid::new_v4().simple());
        let mark = to.mark();
        let sent = Instant::now();
        from.send(&BotCommand::SendChat { text: text.clone() })?;
        let expected = text.clone();
        let (_, received) = to.wait(
            mark,
            EVENT_TIMEOUT,
            "the chat message",
            move |e| matches!(e, BotEvent::ChatReceived { text, .. } if *text == expected),
        )?;
        if let BotEvent::ChatReceived { sender, .. } = received.event
            && sender != from.name()
        {
            bail!(
                "{label}: the message arrived from {sender:?}, expected {:?}",
                from.name()
            );
        }
        ctx.measure_ms(&format!("{label}_latency"), sent);
    }
    Ok(())
}

fn fill_pseudorandom(path: &Path, len: u64, seed: u64) -> anyhow::Result<()> {
    let mut state = seed.max(1);
    let mut writer = BufWriter::new(File::create(path).context("creating a test file")?);
    let mut block = vec![0u8; FILE_BLOCK];
    let mut left = len;
    while left > 0 {
        let n = usize::try_from(left).map_or(FILE_BLOCK, |l| l.min(FILE_BLOCK));
        for piece in block[..n].chunks_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            piece.copy_from_slice(&state.to_le_bytes()[..piece.len()]);
        }
        writer
            .write_all(&block[..n])
            .context("writing a test file")?;
        left -= u64::try_from(n).unwrap_or(left);
    }
    writer.flush().context("writing a test file")
}

fn read_up_to(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Offset of the first byte that differs (or of the end of the shorter file), `None` if identical.
fn first_difference(a: &Path, b: &Path) -> anyhow::Result<Option<u64>> {
    let mut a = File::open(a).context("opening the original file")?;
    let mut b = File::open(b).context("opening the received file")?;
    let mut left = vec![0u8; FILE_BLOCK];
    let mut right = vec![0u8; FILE_BLOCK];
    let mut offset = 0u64;
    loop {
        let n = read_up_to(&mut a, &mut left)?;
        let m = read_up_to(&mut b, &mut right)?;
        let common = n.min(m);
        if let Some(index) = (0..common).find(|&i| left[i] != right[i]) {
            return Ok(Some(offset + u64::try_from(index).unwrap_or(0)));
        }
        if n != m {
            return Ok(Some(offset + u64::try_from(common).unwrap_or(0)));
        }
        if n == 0 {
            return Ok(None);
        }
        offset += u64::try_from(n).unwrap_or(0);
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn send_and_verify(
    from: &BotProcess,
    to: &BotProcess,
    path: &Path,
    mime: &str,
    timeout: Duration,
) -> anyhow::Result<Duration> {
    let name = file_name_of(path);
    let size = fs::metadata(path).context("reading the test file")?.len();
    let to_mark = to.mark();
    let started = Instant::now();
    let from_mark = from.send(&BotCommand::SendFile {
        path: path.to_string_lossy().into_owned(),
    })?;
    let expected = name.clone();
    let (_, received) = to.wait(to_mark, timeout, "the file to arrive", move |e| {
        matches!(e, BotEvent::FileTransfer { name, outgoing: false, state, .. }
            if *name == expected && (state == "done" || state == "failed"))
    })?;
    let elapsed = started.elapsed();
    let BotEvent::FileTransfer {
        sender,
        size: received_size,
        mime: received_mime,
        state,
        path: received_path,
        error,
        ..
    } = received.event
    else {
        bail!("unexpected event while waiting for {name}");
    };
    if state != "done" {
        bail!("{name} failed on {}: {error:?}", to.name());
    }
    if sender != from.name() {
        bail!("{name} arrived from {sender:?}, expected {:?}", from.name());
    }
    if received_size != size || received_mime != mime {
        bail!(
            "{name} arrived as {received_size} bytes of {received_mime}, expected {size} of {mime}"
        );
    }
    let Some(received_path) = received_path else {
        bail!("{name} is done but has no path");
    };
    if let Some(offset) = first_difference(path, Path::new(&received_path))? {
        bail!("{name} differs from the original at byte {offset}");
    }
    let expected = name.clone();
    from.wait(
        from_mark,
        EVENT_TIMEOUT,
        "the outgoing transfer to finish",
        move |e| {
            matches!(e, BotEvent::FileTransfer { name, outgoing: true, state, .. }
                if *name == expected && state == "done")
        },
    )?;
    Ok(elapsed)
}

fn files(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let dir = ctx
        .setup
        .work_dir
        .join(format!("e2e-files-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).context("creating the test files folder")?;
    let outcome = files_in(ctx, &dir);
    if let Err(err) = fs::remove_dir_all(&dir) {
        tracing::debug!(%err, "could not remove the test files");
    }
    outcome
}

fn files_in(ctx: &mut Ctx<'_>, dir: &Path) -> anyhow::Result<()> {
    let room = ctx.room();
    let alice = ctx.spawn_tone("alice", false)?;
    let bob = ctx.spawn_tone("bob", false)?;
    ctx.join(&alice, &room)?;
    ctx.join(&bob, &room)?;
    alice.wait(0, EVENT_TIMEOUT, "alice to see bob", |e| {
        matches!(e, BotEvent::ParticipantJoined { participant } if participant.identity == "bob")
            || matches!(e, BotEvent::Joined { participants, .. } if participants.iter().any(|p| p.identity == "bob"))
    })?;

    let small = dir.join("pequeno.png");
    let big = dir.join("grande.bin");
    let over = dir.join("demais.bin");
    fill_pseudorandom(&small, SMALL_FILE_BYTES, 0x9E37_79B9_7F4A_7C15)?;
    fill_pseudorandom(&big, MAX_FILE_BYTES, 0xD1B5_4A32_D192_ED03)?;
    fill_pseudorandom(&over, MAX_FILE_BYTES + 1, 0x2545_F491_4F6C_DD1D)?;
    ctx.rec.step("test files created");

    let sent = Instant::now();
    send_and_verify(&alice, &bob, &small, "image/png", EVENT_TIMEOUT)?;
    ctx.measure_ms("small_file_alice_to_bob", sent);

    let elapsed = send_and_verify(
        &bob,
        &alice,
        &big,
        "application/octet-stream",
        BIG_FILE_TIMEOUT,
    )?;
    let seconds = elapsed.as_secs_f64();
    ctx.rec.number("big_file_seconds", seconds, "s");
    ctx.rec.number(
        "big_file_rate",
        MAX_FILE_BYTES as f64 / 1_048_576.0 / seconds.max(f64::EPSILON),
        "MB/s",
    );

    let bob_mark = bob.mark();
    let alice_mark = alice.send(&BotCommand::SendFile {
        path: over.to_string_lossy().into_owned(),
    })?;
    alice.wait(
        alice_mark,
        EVENT_TIMEOUT,
        "the oversized file to be refused",
        |e| matches!(e, BotEvent::CommandFailed { error } if error.kind == "invalid_input"),
    )?;
    std::thread::sleep(NO_TRANSFER_WINDOW);
    if bob
        .events_since(bob_mark)
        .iter()
        .any(|s| matches!(s.event, BotEvent::FileTransfer { .. }))
    {
        bail!("bob received a transfer for the oversized file");
    }
    ctx.rec
        .step("the oversized file was refused and never reached bob");
    Ok(())
}

fn share(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    ctx.join(&listener, &room)?;
    let baseline = ctx.baseline()?;
    let tone = ctx.spawn_tone(TONE_BOT, true)?;
    let since = ctx.join(&tone, &room)?;
    ctx.wait_tone_present("before_share", &baseline, since, TONE_WITHIN)?;

    let listener_mark = listener.mark();
    let tone_mark = tone.send(&BotCommand::StartShare)?;
    tone.wait(tone_mark, JOIN_TIMEOUT, "the screen share to start", |e| {
        matches!(e, BotEvent::ScreenShareStarted)
    })?;
    listener.wait(
        listener_mark,
        JOIN_TIMEOUT,
        "the remote share announcement",
        |e| matches!(e, BotEvent::RemoteScreenShareStarted { identity } if identity == TONE_BOT),
    )?;
    ctx.rec.step("the listener saw the remote screen share");

    let watch_mark = listener.send(&BotCommand::Watch {
        identity: TONE_BOT.to_owned(),
    })?;
    listener.wait(
        watch_mark,
        EVENT_TIMEOUT,
        "watching to start",
        |e| matches!(e, BotEvent::WatchingStarted { identity } if identity == TONE_BOT),
    )?;
    let measure_mark = listener.send(&BotCommand::MeasureFrames {
        duration_ms: FRAME_WINDOW_MS,
    })?;
    let watching_since = Instant::now();
    ctx.wait_tone_present("while_watching", &baseline, watching_since, TONE_WITHIN)?;
    let (_, measured) = listener.wait(
        measure_mark,
        Duration::from_millis(FRAME_WINDOW_MS) + EVENT_TIMEOUT,
        "the frame measurement",
        |e| {
            matches!(
                e,
                BotEvent::FrameMeasurement { .. } | BotEvent::FrameMeasurementFailed { .. }
            )
        },
    )?;
    check_frames(ctx, measured.event)?;

    let stop_mark = listener.send(&BotCommand::StopWatching)?;
    listener.wait(stop_mark, EVENT_TIMEOUT, "watching to stop", |e| {
        matches!(e, BotEvent::WatchingStopped)
    })?;
    let listener_mark = listener.mark();
    let tone_mark = tone.send(&BotCommand::StopShare)?;
    tone.wait(tone_mark, EVENT_TIMEOUT, "the share to stop", |e| {
        matches!(e, BotEvent::ScreenShareStopped)
    })?;
    listener.wait(
        listener_mark,
        EVENT_TIMEOUT,
        "the remote share to disappear",
        |e| matches!(e, BotEvent::RemoteScreenShareStopped { identity } if identity == TONE_BOT),
    )?;
    ctx.rec
        .step("stop-watching and stop-share emitted the expected events");
    Ok(())
}

fn check_frames(ctx: &mut Ctx<'_>, event: BotEvent) -> anyhow::Result<()> {
    let BotEvent::FrameMeasurement {
        frames,
        fps,
        width,
        height,
        mean_rgb,
        luma_stddev,
        ..
    } = event
    else {
        bail!("no frame could be measured: {event:?}");
    };
    let (pattern_w, pattern_h) = TEST_PATTERN_SIZE;
    let preset = SharePreset::P720F30;
    let expected = fit_within(
        pattern_w,
        pattern_h,
        preset.max_width(),
        preset.max_height(),
    );
    ctx.rec.number("frames_received", frames as f64, "");
    ctx.rec.number("fps", fps, "fps");
    ctx.rec.text("frame_size", format!("{width}x{height}"));
    ctx.rec.text(
        "expected_frame_size",
        format!("{}x{}", expected.0, expected.1),
    );
    ctx.rec.text(
        "mean_rgb",
        format!("{:.0}, {:.0}, {:.0}", mean_rgb[0], mean_rgb[1], mean_rgb[2]),
    );
    ctx.rec.number("luma_stddev", luma_stddev, "");
    if (width, height) != expected {
        bail!(
            "frame size {width}x{height}, expected {}x{}",
            expected.0,
            expected.1
        );
    }
    if fps < MIN_PLAUSIBLE_FPS {
        bail!("only {fps:.1} frames per second arrived, expected at least {MIN_PLAUSIBLE_FPS}");
    }
    let expected_colour = expected_pattern_mean(width, height);
    if !colour_matches(mean_rgb, expected_colour) {
        bail!(
            "mean colour {:.0?} is far from the test pattern's {:.0?}",
            mean_rgb,
            expected_colour
        );
    }
    if luma_stddev < MIN_LUMA_STDDEV {
        bail!("the frame looks flat (luma stddev {luma_stddev:.1})");
    }
    Ok(())
}

fn room_switch(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let (room_a, room_b) = (ctx.room(), ctx.room_b());
    let baseline = ctx.baseline()?;
    let tone = ctx.spawn_tone(TONE_BOT, false)?;
    ctx.join(&tone, &room_a)?;
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    let since = ctx.join(&listener, &room_a)?;
    let present = ctx.wait_tone_present("first_visit", &baseline, since, TONE_WITHIN)?;

    let tone_mark = tone.mark();
    let switched = Instant::now();
    let mark = listener.send(&BotCommand::Join {
        room: room_b.clone(),
    })?;
    let expected_b = room_b.clone();
    let (_, in_b) = listener.wait(
        mark,
        JOIN_TIMEOUT,
        "the join of the second room",
        move |e| matches!(e, BotEvent::Joined { room, .. } if *room == expected_b),
    )?;
    if let BotEvent::Joined { participants, .. } = in_b.event
        && participants.iter().any(|p| p.identity == TONE_BOT)
    {
        bail!("the second room already lists the tone bot");
    }
    ctx.measure_ms("switch_time", switched);
    tone.wait(
        tone_mark,
        EVENT_TIMEOUT,
        "the tone bot to see the listener leave",
        |e| matches!(e, BotEvent::ParticipantLeft { identity } if identity == LISTENER_BOT),
    )?;
    ctx.wait_tone_absent("other_room", &baseline, &present, switched, ABSENT_WITHIN)?;

    let tone_mark = tone.mark();
    let back = Instant::now();
    let mark = listener.send(&BotCommand::Join {
        room: room_a.clone(),
    })?;
    let (_, again) = listener.wait(
        mark,
        JOIN_TIMEOUT,
        "the return to the first room",
        move |e| matches!(e, BotEvent::Joined { room, .. } if *room == room_a),
    )?;
    match again.event {
        BotEvent::Joined { participants, .. }
            if participants.iter().any(|p| p.identity == TONE_BOT) => {}
        _ => bail!("the first room does not list the tone bot after returning"),
    }
    tone.wait(tone_mark, EVENT_TIMEOUT, "the tone bot to see the listener return", |e| {
        matches!(e, BotEvent::ParticipantJoined { participant } if participant.identity == LISTENER_BOT)
    })?;
    ctx.wait_tone_present("second_visit", &baseline, back, TONE_WITHIN)?;
    Ok(())
}

fn cue(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    const MUTED_HZ: f64 = 880.0;
    const DEAFENED_HZ: f64 = 440.0;
    const WINDOW: Duration = Duration::from_millis(1000);
    const CUE_LEAD: Duration = Duration::from_millis(300);
    const AFTER_WAIT: Duration = Duration::from_millis(500);

    ctx.require_probe()?;
    let room = ctx.room();
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    ctx.join(&listener, &room)?;

    let baseline = ctx.probe.measure_tone(MUTED_HZ, WINDOW)?;
    ctx.rec.number("baseline_tone_rms", baseline.tone_rms, "");
    ctx.rec.number("baseline_total_rms", baseline.total_rms, "");

    for (cue_name, hz) in [("muted", MUTED_HZ), ("deafened", DEAFENED_HZ)] {
        let probe = ctx.probe;
        // The cue is sent after capture starts so it lands mid-window, away from the Hann edges.
        let m = std::thread::scope(|scope| {
            let measuring = scope.spawn(|| probe.measure_tone(hz, WINDOW));
            std::thread::sleep(CUE_LEAD);
            listener.send(&BotCommand::PlayCue {
                cue: cue_name.to_owned(),
            })?;
            measuring
                .join()
                .map_err(|_| anyhow::anyhow!("the measuring thread panicked"))?
        })?;
        ctx.rec
            .number(&format!("{cue_name}_tone_rms"), m.tone_rms, "");
        ctx.rec
            .number(&format!("{cue_name}_total_rms"), m.total_rms, "");
        ctx.rec.number(&format!("{cue_name}_peak"), m.peak, "");
        ctx.rec
            .number(&format!("{cue_name}_tone_ratio"), m.tone_ratio, "");
        if m.tone_rms < MIN_TONE_RMS || m.tone_rms < 5.0 * baseline.tone_rms {
            bail!(
                "cue {cue_name} at {hz} Hz not heard (tone_rms {:.5}, tone_ratio {:.2}, baseline {:.5})",
                m.tone_rms,
                m.tone_ratio,
                baseline.tone_rms
            );
        }
    }

    std::thread::sleep(AFTER_WAIT);
    let after = ctx.probe.measure_tone(MUTED_HZ, WINDOW)?;
    ctx.rec.number("after_tone_rms", after.tone_rms, "");
    if after.tone_rms >= MIN_TONE_RMS {
        bail!(
            "the cue is still audible after stopping (tone_rms {:.5})",
            after.tone_rms
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rooms_protocol::validate_room_name;

    use super::*;

    #[test]
    fn scenario_names_are_unique_and_rooms_are_valid_and_distinct() {
        let mut names: Vec<_> = ALL.iter().map(|d| d.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len());
        let rooms = all_room_names();
        let mut unique = rooms.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), rooms.len());
        for room in rooms {
            assert!(validate_room_name(&room).is_ok(), "{room}");
        }
    }

    #[test]
    fn pseudorandom_files_are_deterministic_and_compared_bytewise() {
        let dir = std::env::temp_dir().join(format!("e2e-files-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let (a, b, c, d) = (dir.join("a"), dir.join("b"), dir.join("c"), dir.join("d"));
        let len = u64::try_from(FILE_BLOCK).unwrap() * 2 + 13;
        fill_pseudorandom(&a, len, 7).unwrap();
        fill_pseudorandom(&b, len, 7).unwrap();
        fill_pseudorandom(&c, len, 8).unwrap();
        fill_pseudorandom(&d, len + 1, 7).unwrap();
        assert_eq!(fs::metadata(&a).unwrap().len(), len);
        assert_eq!(fs::metadata(&d).unwrap().len(), len + 1);
        assert_eq!(first_difference(&a, &b).unwrap(), None);
        assert!(first_difference(&a, &c).unwrap().is_some());
        assert_eq!(first_difference(&a, &d).unwrap(), Some(len));
        let mut bytes = fs::read(&b).unwrap();
        bytes[FILE_BLOCK + 5] ^= 1;
        fs::write(&b, bytes).unwrap();
        assert_eq!(
            first_difference(&a, &b).unwrap(),
            Some(u64::try_from(FILE_BLOCK).unwrap() + 5)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_matches_the_brief() {
        let names: Vec<_> = ALL.iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            [
                "token",
                "presence",
                "playout-listener-first",
                "playout-tone-first",
                "playout-explicit-device",
                "enhanced-mic",
                "maximum-mic",
                "mute",
                "deafen",
                "chat",
                "files",
                "share",
                "share-audio",
                "participant-volume",
                "room-switch",
                "cue",
            ]
        );
        assert!(!ALL[0].needs_media);
    }
}
