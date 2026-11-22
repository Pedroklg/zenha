//! Invite, presence and room switching scenarios.

use std::time::Instant;

use anyhow::bail;

use super::ctx::*;
use super::room_name;
use crate::detect::TONE_HZ;
use crate::proc::{http_request, require_status};
use crate::protocol::{BotCommand, BotEvent};

pub(super) fn token(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

    let mallory = ctx.spawn("mallory", Some(TONE_HZ), false, "convite-errado", None)?;
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

pub(super) fn presence(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

pub(super) fn room_switch(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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
