# Contributing to Zenha

## Requirements

- Windows 10 or 11 (x64, native `x86_64-pc-windows-msvc` target)
- [rustup](https://rustup.rs) (picks the toolchain from [rust-toolchain.toml](rust-toolchain.toml))
- Visual Studio Build Tools with "Desktop development with C++"
- [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) (`cargo install cargo-deny`)
- `livekit-server` (optional, for local media and end-to-end tests)

## Checks

Run before every commit or pull request:

```powershell
cargo fmt --all
cargo ci
```

`cargo ci` runs the crate boundary checks, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `cargo deny check`. GitHub Actions does not build or test the code, so this local run is the only gate.

## Crate boundaries

See [docs/architecture.md](docs/architecture.md) for the diagram.

- `rooms-protocol`: `serde` types and validation only; no I/O, no async.
- `rooms-core`: the only crate that may depend on `livekit` and `libwebrtc`. No SDK types in its public API (`Command`, `Event`, `SessionError` and its own types).
- `rooms-client`: talks to `rooms-core` only through `Command` and `Event` channels; the `egui` UI thread never blocks or does I/O.
- `rooms-token-server`: `axum` server that checks invite codes, issues short-lived LiveKit tokens and keeps rooms and saved chat in SQLite.
- Screen capture sits behind the `FrameSource` trait.

## Code rules

- No `unwrap`, `expect`, `panic!`, `todo!` or `dbg!` outside tests (clippy lints).
- `unsafe` is denied; the only exception is `rooms-core`, with `#[expect(unsafe_code)]` and a `SAFETY:` comment.
- Errors: `thiserror` in libraries, `anyhow` with `.context(...)` in binaries.
- Few comments: explain non-obvious invariants, not what the code does.
- Identifiers and `tracing` logs in English; UI text in Brazilian Portuguese.
- Never log secrets (`Config` has a manual `Debug` that omits them).
- New dependencies go in `[workspace.dependencies]` in the root [Cargo.toml](Cargo.toml) and are used with `workspace = true`. `livekit`, `livekit-api` and `libwebrtc` stay pinned to exact versions (`=x.y.z`).
- Add unit tests for new pure logic (protocol, config, state transitions, DSP).

The full rule set used in review is in [docs/standards/](docs/standards/README.md) (Portuguese).

## Commits

- Keep `main` passing `cargo ci`; work on short-lived branches.
- Use [Conventional Commits](https://www.conventionalcommits.org/): `feat(core): ...`, `fix(client): ...`, `docs: ...`, `chore: ...`.
- One logical change per commit.

## Secrets

- `.env`, `.env.*`, `*.tfvars`, `*.pem`, `*.key` and `deploy/livekit.prod.yaml` are gitignored and must never be committed.
- Document every new server environment variable with a placeholder in [.env.example](.env.example).

## Architecture decisions

Record significant decisions as short ADRs in [docs/adr/](docs/adr/), starting from [0000-template.md](docs/adr/0000-template.md).

## Manual testing on Windows

Unit tests cannot cover audio hardware, GPU rendering, screen capture, hotkeys in fullscreen games or Credential Manager. When you change those areas, test by hand:

- `cargo client -- --demo` shows the UI, chat and a simulated share without a server.
- `scripts\dev-up.cmd` starts `tools\livekit\livekit-server.exe --dev` and the token server; open several clients with `scripts\dev-client.cmd` (or use `cargo server` and `cargo client`).
- `scripts\e2e.cmd` runs the multi-bot end-to-end harness. It plays a quiet test tone on the output device.
- Say in the pull request what you tested by hand and what could not be checked.
