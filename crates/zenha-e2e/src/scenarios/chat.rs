//! Chat and file sending scenarios.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use zenha_protocol::MAX_FILE_BYTES;

use super::ctx::*;
use crate::controller::BotProcess;
use crate::protocol::{BotCommand, BotEvent};

const SMALL_FILE_BYTES: u64 = 200 * 1024;
const BIG_FILE_TIMEOUT: Duration = Duration::from_secs(180);
const NO_TRANSFER_WINDOW: Duration = Duration::from_secs(2);
const FILE_BLOCK: usize = 64 * 1024;

pub(super) fn chat(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

pub(super) fn files(ctx: &mut Ctx<'_>) -> anyhow::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
