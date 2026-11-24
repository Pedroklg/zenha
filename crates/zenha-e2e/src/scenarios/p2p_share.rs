//! Transmissão de tela P2P: troca SFU/P2P, limite de espectadores, limpeza e desligamento.
//!
//! Os bots rodam na mesma máquina, então o ICE fecha por candidatos `host`; NAT real fica para o
//! teste manual entre duas casas.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::bail;

use super::ctx::*;
use super::share::check_frames;
use crate::controller::BotProcess;
use crate::protocol::{BotCommand, BotEvent};

const SHARER: &str = "sharer";
const VIEWER: &str = "viewer";
const SECOND_VIEWER: &str = "viewer-2";
const FRAME_WINDOW_MS: u64 = 3000;
/// Covers ICE on loopback plus the viewer's 10 s NAT timeout.
const OUTCOME_WITHIN: Duration = Duration::from_secs(15);
const LOG_POLL: Duration = Duration::from_millis(250);
/// The sharer accepts one request per viewer every 5 s.
const REQUEST_COOLDOWN: Duration = Duration::from_millis(5500);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    session_id: String,
    outcome: String,
    reason: String,
}

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.split_once(&format!(" {name}="))?.1;
    let value = rest.split_whitespace().next()?;
    Some(value.trim_matches('"'))
}

fn outcomes(log: &[String]) -> Vec<Outcome> {
    log.iter()
        .filter(|line| line.contains("p2p outcome") && line.contains("role=\"receiver\""))
        .filter_map(|line| {
            Some(Outcome {
                session_id: field(line, "session_id")?.to_owned(),
                outcome: field(line, "outcome")?.to_owned(),
                reason: field(line, "reason")?.to_owned(),
            })
        })
        .collect()
}

fn sender_closes(log: &[String]) -> Vec<(String, String)> {
    log.iter()
        .filter(|line| line.contains("p2p sender closed"))
        .filter_map(|line| {
            Some((
                field(line, "reason")?.to_owned(),
                field(line, "active_p2p_viewers")?.to_owned(),
            ))
        })
        .collect()
}

fn wait_log<T>(
    bot: &BotProcess,
    what: &str,
    found: impl Fn(&[String]) -> Option<T>,
) -> anyhow::Result<T> {
    let deadline = Instant::now() + OUTCOME_WITHIN;
    loop {
        if let Some(value) = found(&bot.log_tail(usize::MAX)) {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            bail!(
                "{} did not log {what} within {:.0} s",
                bot.name(),
                OUTCOME_WITHIN.as_secs_f64()
            );
        }
        std::thread::sleep(LOG_POLL);
    }
}

/// Waits for the `nth` receiver outcome (0-based) and returns it.
fn wait_outcome(bot: &BotProcess, nth: usize) -> anyhow::Result<Outcome> {
    wait_log(bot, "a p2p outcome", |log| outcomes(log).get(nth).cloned())
}

/// Waits for P2P to come up again on a session other than `previous`.
fn wait_new_p2p(ctx: &mut Ctx<'_>, bot: &BotProcess, previous: &str) -> anyhow::Result<Outcome> {
    let found = wait_log(bot, "p2p on a new session", |log| {
        outcomes(log)
            .into_iter()
            .find(|o| o.outcome == "p2p_active" && o.session_id != previous)
    })?;
    ctx.rec.step(format!(
        "{}: p2p_active again, session {}",
        bot.name(),
        found.session_id
    ));
    Ok(found)
}

fn start_share(sharer: &BotProcess, viewers: &[&BotProcess]) -> anyhow::Result<()> {
    let marks: Vec<usize> = viewers.iter().map(|viewer| viewer.mark()).collect();
    let mark = sharer.send(&BotCommand::StartShare { preset: None })?;
    sharer.wait(mark, JOIN_TIMEOUT, "the screen share to start", |e| {
        matches!(e, BotEvent::ScreenShareStarted)
    })?;
    for (viewer, mark) in viewers.iter().zip(marks) {
        viewer.wait(
            mark,
            JOIN_TIMEOUT,
            "the remote share announcement",
            |e| matches!(e, BotEvent::RemoteScreenShareStarted { identity } if identity == SHARER),
        )?;
    }
    Ok(())
}

fn watch(viewer: &BotProcess) -> anyhow::Result<()> {
    let mark = viewer.send(&BotCommand::Watch {
        identity: SHARER.to_owned(),
    })?;
    viewer.wait(
        mark,
        EVENT_TIMEOUT,
        "watching to start",
        |e| matches!(e, BotEvent::WatchingStarted { identity } if identity == SHARER),
    )?;
    Ok(())
}

fn stop_watching(viewer: &BotProcess) -> anyhow::Result<()> {
    let mark = viewer.send(&BotCommand::StopWatching)?;
    viewer.wait(mark, EVENT_TIMEOUT, "watching to stop", |e| {
        matches!(e, BotEvent::WatchingStopped)
    })?;
    Ok(())
}

fn measure(ctx: &mut Ctx<'_>, viewer: &BotProcess) -> anyhow::Result<()> {
    let mark = viewer.send(&BotCommand::MeasureFrames {
        duration_ms: FRAME_WINDOW_MS,
    })?;
    let (_, measured) = viewer.wait(
        mark,
        Duration::from_millis(FRAME_WINDOW_MS) + EVENT_TIMEOUT,
        "the frame measurement",
        |e| {
            matches!(
                e,
                BotEvent::FrameMeasurement { .. } | BotEvent::FrameMeasurementFailed { .. }
            )
        },
    )?;
    check_frames(ctx, measured.event)
}

fn expect_outcome(
    ctx: &mut Ctx<'_>,
    viewer: &BotProcess,
    nth: usize,
    outcome: &str,
    reason: &str,
) -> anyhow::Result<Outcome> {
    let found = wait_outcome(viewer, nth)?;
    if found.outcome != outcome || found.reason != reason {
        bail!(
            "{} logged {}/{} instead of {outcome}/{reason}",
            viewer.name(),
            found.outcome,
            found.reason
        );
    }
    ctx.rec.step(format!(
        "{}: {outcome} ({reason}), session {}",
        viewer.name(),
        found.session_id
    ));
    Ok(found)
}

fn sharer_and_viewer(
    ctx: &mut Ctx<'_>,
    sharer_p2p: bool,
    viewer_p2p: bool,
) -> anyhow::Result<(Arc<BotProcess>, Arc<BotProcess>)> {
    let room = ctx.room();
    let sharer = ctx.spawn_p2p(SHARER, true, sharer_p2p)?;
    sharer.send(&BotCommand::SetMuted { muted: true })?;
    ctx.join(&sharer, &room)?;
    let viewer = ctx.spawn_p2p(VIEWER, false, viewer_p2p)?;
    viewer.send(&BotCommand::SetMuted { muted: true })?;
    ctx.join(&viewer, &room)?;
    start_share(&sharer, &[&viewer])?;
    Ok((sharer, viewer))
}

pub(super) fn p2p_upgrade(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let (_sharer, viewer) = sharer_and_viewer(ctx, true, true)?;
    let since = Instant::now();
    watch(&viewer)?;
    expect_outcome(ctx, &viewer, 0, "p2p_active", "none")?;
    ctx.measure_ms("time_to_p2p", since);
    measure(ctx, &viewer)?;
    if outcomes(&viewer.log_tail(usize::MAX)).len() > 1 {
        bail!("the viewer left p2p while frames were measured");
    }
    stop_watching(&viewer)
}

pub(super) fn p2p_viewer_limit(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let (sharer, viewer) = sharer_and_viewer(ctx, true, true)?;
    watch(&viewer)?;
    expect_outcome(ctx, &viewer, 0, "p2p_active", "none")?;

    let second = ctx.spawn_p2p(SECOND_VIEWER, false, true)?;
    second.send(&BotCommand::SetMuted { muted: true })?;
    let room = ctx.room();
    ctx.join(&second, &room)?;
    watch(&second)?;
    expect_outcome(ctx, &second, 0, "sfu_fallback", "limit_exceeded")?;
    expect_outcome(ctx, &viewer, 1, "sfu_fallback", "limit_exceeded")?;
    wait_log(&sharer, "the drained sender closing", |log| {
        sender_closes(log)
            .into_iter()
            .find(|(reason, active)| reason == "limit_exceeded" && active == "0")
    })?;
    measure(ctx, &viewer)?;
    measure(ctx, &second)
}

pub(super) fn p2p_cleanup(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let (sharer, viewer) = sharer_and_viewer(ctx, true, true)?;
    watch(&viewer)?;
    let first = expect_outcome(ctx, &viewer, 0, "p2p_active", "none")?;
    let mark = sharer.send(&BotCommand::StopShare)?;
    sharer.wait(mark, EVENT_TIMEOUT, "the share to stop", |e| {
        matches!(e, BotEvent::ScreenShareStopped)
    })?;
    wait_log(&sharer, "the sender closing on stop share", |log| {
        sender_closes(log)
            .into_iter()
            .find(|(reason, active)| reason == "share_stopped" && active == "0")
    })?;
    ctx.rec.step("stop share closed the p2p sender");

    std::thread::sleep(REQUEST_COOLDOWN);
    start_share(&sharer, &[&viewer])?;
    watch(&viewer)?;
    wait_new_p2p(ctx, &viewer, &first.session_id)?;
    let mark = viewer.send(&BotCommand::Leave)?;
    viewer.wait(mark, EVENT_TIMEOUT, "the viewer to leave", |e| {
        matches!(e, BotEvent::Left)
    })?;
    wait_log(
        &sharer,
        "the sender closing when the viewer leaves",
        |log| {
            sender_closes(log)
                .into_iter()
                .find(|(reason, active)| reason == "viewer_left" && active == "0")
        },
    )?;
    ctx.rec.step("the viewer leaving closed the p2p sender");
    Ok(())
}

pub(super) fn p2p_disabled(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let (sharer, viewer) = sharer_and_viewer(ctx, true, false)?;
    watch(&viewer)?;
    measure(ctx, &viewer)?;
    if !outcomes(&viewer.log_tail(usize::MAX)).is_empty()
        || sharer
            .log_tail(usize::MAX)
            .iter()
            .any(|line| line.contains("p2p offer sent"))
    {
        bail!("a viewer with p2p off still negotiated p2p");
    }
    ctx.rec.step("viewer with p2p off stayed on the sfu");
    stop_watching(&viewer)?;

    let viewer_on = ctx.spawn_p2p(SECOND_VIEWER, false, true)?;
    let room = ctx.room();
    ctx.join(&viewer_on, &room)?;
    let mark = sharer.send(&BotCommand::StopShare)?;
    sharer.wait(mark, EVENT_TIMEOUT, "the share to stop", |e| {
        matches!(e, BotEvent::ScreenShareStopped)
    })?;
    sharer.quit();
    let sharer_off = ctx.spawn_p2p("sharer-off", true, false)?;
    ctx.join(&sharer_off, &room)?;
    let viewer_mark = viewer_on.mark();
    let mark = sharer_off.send(&BotCommand::StartShare { preset: None })?;
    sharer_off.wait(mark, JOIN_TIMEOUT, "the screen share to start", |e| {
        matches!(e, BotEvent::ScreenShareStarted)
    })?;
    viewer_on.wait(
        viewer_mark,
        JOIN_TIMEOUT,
        "the remote share announcement",
        |e| matches!(e, BotEvent::RemoteScreenShareStarted { identity } if identity == "sharer-off"),
    )?;
    let mark = viewer_on.send(&BotCommand::Watch {
        identity: "sharer-off".to_owned(),
    })?;
    viewer_on.wait(mark, EVENT_TIMEOUT, "watching to start", |e| {
        matches!(e, BotEvent::WatchingStarted { .. })
    })?;
    expect_outcome(ctx, &viewer_on, 0, "sfu_fallback", "p2p_disabled")?;
    measure(ctx, &viewer_on)
}

pub(super) fn p2p_rewatch(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let (_sharer, viewer) = sharer_and_viewer(ctx, true, true)?;
    watch(&viewer)?;
    let first = expect_outcome(ctx, &viewer, 0, "p2p_active", "none")?;
    stop_watching(&viewer)?;
    std::thread::sleep(REQUEST_COOLDOWN);
    watch(&viewer)?;
    wait_new_p2p(ctx, &viewer, &first.session_id)?;
    measure(ctx, &viewer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_and_close_lines_are_parsed_from_the_bot_log() {
        let log = [
            "2026-11-23T10:00:00Z  INFO zenha_core::livekit_backend::p2p::coordinator: p2p outcome role=\"receiver\" peer=\"sharer\" session_id=\"abc\" outcome=\"p2p_active\" reason=\"none\" route=\"ipv4_host\"",
            "2026-11-23T10:00:01Z  INFO zenha_core::livekit_backend::p2p::coordinator: p2p outcome role=\"sender\" peer=\"viewer\" session_id=\"abc\" outcome=\"p2p_active\" reason=\"none\" route=\"none\"",
            "2026-11-23T10:00:02Z  INFO zenha_core::livekit_backend::p2p::coordinator: p2p sender closed role=\"sender\" peer=\"viewer\" session_id=abc reason=\"share_stopped\" route=\"ipv4_host\" active_p2p_viewers=0",
        ]
        .map(str::to_owned);
        assert_eq!(
            outcomes(&log),
            vec![Outcome {
                session_id: "abc".to_owned(),
                outcome: "p2p_active".to_owned(),
                reason: "none".to_owned(),
            }]
        );
        assert_eq!(
            sender_closes(&log),
            vec![("share_stopped".to_owned(), "0".to_owned())]
        );
    }
}
