//! Contexto compartilhado dos cenários: ambiente, bots, sonda e esperas.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use super::{ScenarioDef, room_name, second_room_name};
use crate::controller::{BotProcess, BotSpec};
use crate::detect::{TONE_AMPLITUDE, TONE_HZ, baseline_is_noisy, tone_absent, tone_present};
use crate::dsp::ToneMeasurement;
use crate::probe::{AudioProbe, RenderDevice, match_sdk_output};
use crate::protocol::{BotCommand, BotEvent, DeviceInfo};
use crate::report::Environment;
use crate::scenario::{BotLog, Recorder, ScenarioResult};

pub(super) const JOIN_TIMEOUT: Duration = Duration::from_secs(25);
pub(super) const EVENT_TIMEOUT: Duration = Duration::from_secs(10);
const BASELINE_WINDOW: Duration = Duration::from_millis(1000);
const POLL_WINDOW: Duration = Duration::from_millis(500);
pub(super) const TONE_WITHIN: Duration = Duration::from_secs(13);
pub(super) const ABSENT_WITHIN: Duration = Duration::from_secs(8);
const LOG_TAIL_LINES: usize = 30;
pub(super) const TONE_BOT: &str = "tone";
pub(super) const LISTENER_BOT: &str = "listener";

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

pub(super) struct ListenerDevices {
    pub(super) inputs: Vec<DeviceInfo>,
    pub(super) outputs: Vec<DeviceInfo>,
    pub(super) selected_output: Option<String>,
}

pub struct Ctx<'a> {
    pub rec: Recorder,
    pub(super) setup: &'a Setup,
    pub(super) probe: &'a dyn AudioProbe,
    pub(super) env: &'a mut Environment,
    pub(super) bots: Vec<Arc<BotProcess>>,
    pub(super) name: &'static str,
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

    pub(super) fn room(&self) -> String {
        room_name(self.name)
    }

    pub(super) fn room_b(&self) -> String {
        second_room_name(self.name)
    }

    pub(super) fn spawn(
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

    pub(super) fn spawn_tone(&mut self, name: &str, pattern: bool) -> anyhow::Result<Arc<BotProcess>> {
        let invite = self.setup.invite.clone();
        self.spawn(name, true, pattern, &invite)
    }

    pub(super) fn spawn_listener(&mut self, name: &str) -> anyhow::Result<Arc<BotProcess>> {
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

    pub(super) fn select_output(&mut self, bot: &BotProcess, device: &RenderDevice) -> anyhow::Result<()> {
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

    pub(super) fn refresh_devices(&mut self, bot: &BotProcess) -> anyhow::Result<ListenerDevices> {
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

    pub(super) fn join(&mut self, bot: &BotProcess, room: &str) -> anyhow::Result<Instant> {
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

    pub(super) fn measure_ms(&mut self, name: &str, since: Instant) {
        self.rec
            .number(name, since.elapsed().as_secs_f64() * 1000.0, "ms");
    }

    pub(super) fn require_probe(&self) -> anyhow::Result<()> {
        self.probe
            .render_devices()
            .map(|_| ())
            .context("the audio probe is not working")
    }

    pub(super) fn baseline(&mut self) -> anyhow::Result<ToneMeasurement> {
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

    pub(super) fn wait_tone_present(
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

    pub(super) fn wait_tone_absent(
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

