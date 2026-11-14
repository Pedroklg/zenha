# Contributing to Zenha

Thank you for your interest in contributing to Zenha. Please review these guidelines before opening a pull request.

## Requirements

- Windows 10 or 11 (x64, native `x86_64-pc-windows-msvc` target)
- [rustup](https://rustup.rs) (automatically selects the toolchain from [rust-toolchain.toml](rust-toolchain.toml))
- Visual Studio Build Tools with **"Desktop development with C++"**
- `livekit-server` binary (optional, for local media testing and end-to-end tests)

## Verification (`cargo ci`)

Run the full check suite before every commit or pull request:

```powershell
cargo fmt --all
cargo ci
```

`cargo ci` runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.

## Code & Architecture Rules (`CLAUDE.md`)

The repository includes `CLAUDE.md` files at the root and inside each crate to guide both contributors and AI coding agents. The key rules are summarized below:

### Crate Boundaries

- `rooms-protocol`: `serde` types and size/input validation only; no I/O or async code.
- `rooms-core`: the **only** crate allowed to depend on `livekit` and `libwebrtc`. No SDK types may appear in its public API (`Command`, `Event`, `SessionError`, and domain types).
- `rooms-client`: communicates with `rooms-core` exclusively over `Command` and `Event` channels; the `egui` UI thread must never block or perform I/O.
- `rooms-token-server`: stateless HTTP server (`axum`) that validates invite codes and issues short-lived LiveKit tokens.
- Screen capture sits behind the `FrameSource` trait so capturers can be swapped cleanly.

### Coding Standards

- No `unwrap`, `expect`, `panic!`, `todo!`, or `dbg!` outside tests (enforced by workspace clippy lints).
- `unsafe` is denied by workspace lint; exceptions are only permitted inside `rooms-core` with `#[expect(unsafe_code)]` and a `SAFETY:` comment.
- Error handling: `thiserror` in library crates, `anyhow` with `.context(...)` in binaries.
- Keep comments minimal: explain *why* for non-obvious invariants, not *what* the code does.
- Code identifiers and `tracing` logs are in English (lowercase, structured fields); user-facing UI strings are in Brazilian Portuguese.
- Never log secrets (`Config` uses a manual `Debug` implementation that redacts them).
- Add new dependencies under `[workspace.dependencies]` in the root [Cargo.toml](Cargo.toml) and reference them with `workspace = true`. Keep `livekit`, `livekit-api`, and `libwebrtc` pinned to exact versions (`=x.y.z`) and update them deliberately.
- Add unit tests for any new pure logic (protocol, configuration, state transitions, DSP).

## Git & Conventional Commits

- Keep `main` compiling and passing `cargo ci` at all times; develop on short-lived branches.
- Use [Conventional Commits](https://www.conventionalcommits.org/): e.g., `feat(core): ...`, `fix(client): ...`, `docs: ...`, `chore: ...`.
- One logical change per commit.

## Secrets

- `.env`, `.env.*`, `*.tfvars`, `*.pem`, `*.key`, and `deploy/livekit.prod.yaml` are gitignored and must never be committed.
- Any new server environment variable must be documented with a placeholder in [.env.example](.env.example).

## Architecture Decisions

Significant architectural decisions are recorded as short ADRs under [docs/adr/](docs/adr/) using [0000-template.md](docs/adr/0000-template.md).

## Manual Testing on Windows

Automated unit tests cannot cover real audio hardware, GPU rendering, screen capture, global hotkeys in fullscreen games, or Windows Credential Manager behavior. When changing those areas, test manually on Windows:

- Use `cargo client -- --demo` to quickly inspect UI layout, chat, and simulated screen sharing without running a server.
- Run `scripts\dev-up.cmd` and multiple `scripts\dev-client.cmd` instances (or `cargo server` + `cargo client`) for multi-client voice, video, and file-transfer checks.
- Run `scripts\e2e.cmd` for automated multi-bot verification with WASAPI loopback audio probing (note: plays a low-volume test tone on the Windows output device during execution).
- Explicitly note in your pull request what was tested manually and what could not be verified automatically.
