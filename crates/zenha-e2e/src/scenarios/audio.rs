//! Audio scenarios: output, microphone, mute, deafen, volume and confirmation sounds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::bail;

use super::ctx::*;
use crate::controller::BotProcess;
use crate::detect::{MIN_TONE_RMS, TONE_AMPLITUDE, TONE_HZ};
use crate::dsp::ToneMeasurement;
use crate::protocol::{BotCommand, BotEvent, NoiseKind};

pub(super) fn playout_listener_first(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    ctx.join(&listener, &room)?;
    let baseline = ctx.baseline()?;
    let tone = ctx.spawn_tone(TONE_BOT, false)?;
    let since = ctx.join(&tone, &room)?;
    ctx.wait_tone_present("playout", &baseline, since, TONE_WITHIN)?;
    Ok(())
}

pub(super) fn playout_tone_first(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let baseline = ctx.baseline()?;
    let tone = ctx.spawn_tone(TONE_BOT, false)?;
    ctx.join(&tone, &room)?;
    let listener = ctx.spawn_listener(LISTENER_BOT)?;
    let since = ctx.join(&listener, &room)?;
    ctx.wait_tone_present("playout", &baseline, since, TONE_WITHIN)?;
    Ok(())
}

pub(super) fn playout_explicit_device(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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
pub(super) struct AudibleRoom {
    baseline: ToneMeasurement,
    present: ToneMeasurement,
    tone: Arc<BotProcess>,
    listener: Arc<BotProcess>,
}

pub(super) fn audible_room(ctx: &mut Ctx<'_>) -> anyhow::Result<AudibleRoom> {
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
pub(super) fn audio_taps(log: &[String], identity: &str, kind: &str) -> Vec<(f64, f64)> {
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
pub(super) fn enhanced_mic(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    own_capture_mic(ctx, NoiseKind::Enhanced).map(|_| ())
}

pub(super) fn maximum_mic(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

pub(super) fn own_capture_mic(
    ctx: &mut Ctx<'_>,
    mode: NoiseKind,
) -> anyhow::Result<Arc<BotProcess>> {
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

pub(super) fn mute(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

pub(super) fn participant_volume(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

pub(super) fn deafen(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

pub(super) fn cue(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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
