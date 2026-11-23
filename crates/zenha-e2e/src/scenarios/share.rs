//! Screen sharing scenarios and the accompanying sound.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use zenha_core::{SharePreset, TEST_PATTERN_SIZE, VideoCodec, fit_within};

use super::audio::audio_taps;
use super::ctx::*;
use crate::controller::{BotSpec, SideTone};
use crate::detect::{MIN_TONE_RMS, TONE_AMPLITUDE, TONE_HZ};
use crate::frames::{MIN_LUMA_STDDEV, MIN_PLAUSIBLE_FPS, colour_matches, expected_pattern_mean};
use crate::proc::{compute_cpu_usage, logical_cores, sample_process_cpu};
use crate::protocol::{BotCommand, BotEvent, SharePresetKind};

const FRAME_WINDOW_MS: u64 = 3000;
const DECODE_WINDOW_MS: u64 = 30_000;
const DECODE_SETTLE: Duration = Duration::from_secs(2);
const DECODE_SIZE: (u32, u32) = (1920, 1080);
const MIN_DECODE_FPS: f64 = 1.0;

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
    let share_mark = sharer.send(&BotCommand::StartShare { preset: None })?;
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
    let tone_mark = tone.send(&BotCommand::StartShare { preset: None })?;
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

pub(super) fn av1_decode(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
    let room = ctx.room();
    let cores = logical_cores();
    ctx.rec.number("logical_cores", cores as f64, "núcleos");
    for codec in [VideoCodec::Av1, VideoCodec::H264, VideoCodec::Vp9] {
        measure_codec_decode(ctx, &room, codec, cores)?;
    }
    Ok(())
}

fn measure_codec_decode(
    ctx: &mut Ctx<'_>,
    room: &str,
    codec: VideoCodec,
    cores: usize,
) -> anyhow::Result<()> {
    let sharer_name = format!("sharer-{}", codec.as_str());
    let sharer = ctx.spawn_with(BotSpec {
        name: sharer_name.clone(),
        token_url: ctx.setup.token_url(),
        invite: ctx.setup.invite.clone(),
        tone: Some((TONE_HZ, TONE_AMPLITUDE)),
        test_pattern: true,
        test_pattern_size: Some(DECODE_SIZE),
        video_codec: codec,
        echo_test: None,
    })?;
    sharer.send(&BotCommand::SetMuted { muted: true })?;
    ctx.join(&sharer, room)?;

    let watcher = ctx.spawn_with(BotSpec {
        name: format!("watcher-{}", codec.as_str()),
        token_url: ctx.setup.token_url(),
        invite: ctx.setup.invite.clone(),
        tone: Some((TONE_HZ, TONE_AMPLITUDE)),
        test_pattern: false,
        test_pattern_size: None,
        video_codec: VideoCodec::Vp9,
        echo_test: None,
    })?;
    watcher.send(&BotCommand::SetMuted { muted: true })?;
    watcher.send(&BotCommand::SetDeafened { deafened: true })?;
    ctx.join(&watcher, room)?;
    let watcher_pid = watcher.pid().context("watcher bot has no process id")?;

    for (preset, fps_label) in [
        (SharePresetKind::P1080f30, 30),
        (SharePresetKind::P1080f60, 60),
    ] {
        let tag = format!("{}_{fps_label}", codec.as_str());
        let watcher_mark = watcher.mark();
        let share_mark = sharer.send(&BotCommand::StartShare {
            preset: Some(preset),
        })?;
        sharer.wait(share_mark, JOIN_TIMEOUT, "the screen share to start", |e| {
            matches!(e, BotEvent::ScreenShareStarted)
        })?;
        watcher.wait(
            watcher_mark,
            JOIN_TIMEOUT,
            "the remote share announcement",
            |e| matches!(e, BotEvent::RemoteScreenShareStarted { identity } if identity == &sharer_name),
        )?;

        let watch_mark = watcher.send(&BotCommand::Watch {
            identity: sharer_name.clone(),
        })?;
        watcher.wait(
            watch_mark,
            EVENT_TIMEOUT,
            "watching to start",
            |e| matches!(e, BotEvent::WatchingStarted { identity } if identity == &sharer_name),
        )?;

        std::thread::sleep(DECODE_SETTLE);
        let cpu_before = sample_process_cpu(watcher_pid)?;
        let measure_mark = watcher.send(&BotCommand::MeasureFrames {
            duration_ms: DECODE_WINDOW_MS,
        })?;
        let (_, measured) = watcher.wait(
            measure_mark,
            Duration::from_millis(DECODE_WINDOW_MS) + JOIN_TIMEOUT,
            "the 30s frame measurement",
            |e| {
                matches!(
                    e,
                    BotEvent::FrameMeasurement { .. } | BotEvent::FrameMeasurementFailed { .. }
                )
            },
        )?;
        let cpu_after = sample_process_cpu(watcher_pid)?;
        let cpu = compute_cpu_usage(cpu_before, cpu_after, cores)?;

        if let BotEvent::FrameMeasurement {
            frames,
            fps,
            width,
            height,
            gap_p95_ms,
            gap_max_ms,
            stutters,
            ..
        } = &measured.event
        {
            ctx.rec.number(&format!("{tag}_fps"), *fps, "fps");
            ctx.rec
                .number(&format!("{tag}_gap_p95_ms"), *gap_p95_ms, "ms");
            ctx.rec
                .number(&format!("{tag}_stutters"), f64::from(*stutters), "");
            ctx.rec
                .number(&format!("{tag}_cpu_per_core_pct"), cpu.per_core_pct, "%");
            ctx.rec
                .number(&format!("{tag}_cpu_cores"), cpu.cores_used, "núcleos");
            ctx.rec.number(&format!("{tag}_cpu_s"), cpu.cpu_secs, "s");
            ctx.rec.number(&format!("{tag}_wall_s"), cpu.wall_secs, "s");
            ctx.rec.number(&format!("{tag}_frames"), *frames as f64, "");
            ctx.rec
                .number(&format!("{tag}_gap_max_ms"), *gap_max_ms, "ms");
            ctx.rec
                .text(&format!("{tag}_size"), format!("{width}x{height}"));
            ctx.rec.step(format!(
                "{tag} (1080p@{fps_label}): {fps:.1} fps, gap_p95={gap_p95_ms:.1} ms, engasgos={stutters}, cpu={:.2}%/núcleo ({:.3} núcleos)",
                cpu.per_core_pct, cpu.cores_used
            ));
        }

        let stop_watch = watcher.send(&BotCommand::StopWatching)?;
        watcher.wait(stop_watch, EVENT_TIMEOUT, "watching to stop", |e| {
            matches!(e, BotEvent::WatchingStopped)
        })?;
        let watcher_mark = watcher.mark();
        let stop_share = sharer.send(&BotCommand::StopShare)?;
        sharer.wait(stop_share, EVENT_TIMEOUT, "the share to stop", |e| {
            matches!(e, BotEvent::ScreenShareStopped)
        })?;
        watcher.wait(
            watcher_mark,
            EVENT_TIMEOUT,
            "the remote share to disappear",
            |e| matches!(e, BotEvent::RemoteScreenShareStopped { identity } if identity == &sharer_name),
        )?;

        validate_decode_frames(&tag, &measured.event)?;
    }

    watcher.quit();
    sharer.quit();
    Ok(())
}

fn validate_decode_frames(label: &str, event: &BotEvent) -> anyhow::Result<()> {
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
        bail!("{label}: no frame could be measured: {event:?}");
    };
    if *frames == 0 || *fps < MIN_DECODE_FPS {
        bail!(
            "{label}: only {fps:.1} fps ({frames} frames) arrived, expected at least {MIN_DECODE_FPS}"
        );
    }
    if (*width, *height) != DECODE_SIZE {
        bail!(
            "{label}: frame size {width}x{height}, expected {}x{}",
            DECODE_SIZE.0,
            DECODE_SIZE.1
        );
    }
    let expected_colour = expected_pattern_mean(*width, *height);
    if !colour_matches(*mean_rgb, expected_colour) {
        bail!(
            "{label}: mean colour {:.0?} is far from the test pattern's {:.0?}",
            mean_rgb,
            expected_colour
        );
    }
    if *luma_stddev < MIN_LUMA_STDDEV {
        bail!("{label}: the frame looks flat (luma stddev {luma_stddev:.1})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_decode_event(width: u32, height: u32) -> BotEvent {
        BotEvent::FrameMeasurement {
            identity: "sharer-av1".to_owned(),
            frames: 900,
            seconds: 30.0,
            fps: 30.0,
            width,
            height,
            mean_rgb: expected_pattern_mean(width, height),
            luma_stddev: 80.0,
            gap_p95_ms: 35.0,
            gap_max_ms: 42.0,
            stutters: 0,
        }
    }

    #[test]
    fn decode_validation_accepts_1080p_pattern_and_rejects_missing_or_wrong_frames() {
        assert!(validate_decode_frames("av1_30", &valid_decode_event(1920, 1080)).is_ok());

        assert!(
            validate_decode_frames(
                "av1_30",
                &BotEvent::FrameMeasurementFailed {
                    reason: "no frames".to_owned()
                }
            )
            .is_err()
        );

        let mut zero = valid_decode_event(1920, 1080);
        if let BotEvent::FrameMeasurement { frames, fps, .. } = &mut zero {
            *frames = 0;
            *fps = 0.0;
        }
        assert!(validate_decode_frames("av1_30", &zero).is_err());

        assert!(validate_decode_frames("av1_30", &valid_decode_event(1280, 720)).is_err());

        let mut black = valid_decode_event(1920, 1080);
        if let BotEvent::FrameMeasurement {
            mean_rgb,
            luma_stddev,
            ..
        } = &mut black
        {
            *mean_rgb = [0.0, 0.0, 0.0];
            *luma_stddev = 0.0;
        }
        assert!(validate_decode_frames("av1_30", &black).is_err());
    }
}
