//! Cenários de transmissão de tela e do som que a acompanha.

use std::time::{Duration, Instant};

use anyhow::bail;
use zenha_core::{SharePreset, TEST_PATTERN_SIZE, fit_within};

use super::audio::audio_taps;
use super::ctx::*;
use crate::controller::SideTone;
use crate::detect::MIN_TONE_RMS;
use crate::frames::{MIN_LUMA_STDDEV, MIN_PLAUSIBLE_FPS, colour_matches, expected_pattern_mean};
use crate::protocol::{BotCommand, BotEvent};

const FRAME_WINDOW_MS: u64 = 3000;

pub(super) fn share_audio(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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
pub(super) fn share(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

pub(super) fn check_frames(ctx: &mut Ctx<'_>, event: BotEvent) -> anyhow::Result<()> {
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

