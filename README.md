# Zenha

Zenha is a lightweight Windows desktop app for voice rooms, text chat, file sharing, and screen sharing, built in Rust for a small group of friends. Media runs through a self-hosted [LiveKit](https://github.com/livekit/livekit) SFU, and all custom code in this repository is written in Rust.

> Note: Public documentation is in English, while the desktop UI is currently in Brazilian Portuguese.

<!-- screenshot -->

## Features

- **Voice rooms**: fixed rooms with live occupant lists, active-speaker highlight, mute/deafen controls, input/output device selection, and automatic reconnection.
- **Noise suppression & gate**: choose between Off, WebRTC Default, Enhanced (RNNoise), and Maximum (DeepFilterNet 3), plus an adjustable noise gate with a live microphone level meter.
- **Global hotkeys & push-to-talk**: system-wide mute, deafen, and push-to-talk keybinds with audio feedback cues.
- **Screen & game audio sharing**: share a monitor or window (720p30, 1080p30, or experimental 1080p60) with per-app or system audio loopback and an independent viewer volume slider.
- **Text chat & file sharing**: reliable room chat with unread badge, inline image thumbnails, and file transfers up to 50 MB.
- **Invite links & tray support**: join via `zenha://` invite links, store server passwords in Windows Credential Manager, and minimize/close to the system tray without leaving the call.

## Download

1. Download the latest release from [GitHub Releases](https://github.com/Pedroklg/zenha/releases):
   - **Installer** (`Zenha-<version>-windows-x64-setup.exe`): installs per-user in `%LOCALAPPDATA%\Programs\Zenha` (no administrator privileges required), adds a Start Menu shortcut, and registers the `zenha://` URL protocol.
   - **Portable** (`Zenha-<version>-windows-x64.zip`): extract and run `zenha.exe`.
2. Because release binaries are not code-signed yet, Windows SmartScreen will display **"Windows protected your PC"** on first launch. Click **"More info"** and then **"Run anyway"**.

User settings are stored in `%APPDATA%\zenha` and are kept when uninstalling. Server passwords are stored in the Windows Credential Manager.

## Self-Hosting a Server

To deploy your own `livekit-server` and `rooms-token-server` instance (Docker Compose, Caddy TLS, and firewall setup), see the [deployment guide](docs/deploy.md).

## Building from Source

### Requirements

- Windows 10 or 11 (x64)
- [rustup](https://rustup.rs) (the toolchain version in [rust-toolchain.toml](rust-toolchain.toml) is installed automatically)
- Visual Studio Build Tools with the **"Desktop development with C++"** workload
- A `livekit-server` binary for local media testing

### Quick Start

```powershell
Copy-Item .env.example .env
cargo ci
livekit-server --dev
cargo server
cargo client            # connects to local token server (no media without livekit-server)
cargo client -- --demo  # simulated rooms and participants, no server required
```

### Commands

| Command | Description |
| --- | --- |
| `cargo ci` | Runs boundary checks, formatting check, clippy (`-D warnings`), workspace tests and `cargo deny` |
| `cargo lint` | Runs clippy across all workspace targets |
| `cargo server` | Starts `rooms-token-server` (`cargo run -p rooms-token-server -- hash-password` generates `ROOMS_INVITE_CODE_HASH`) |
| `cargo client` | Launches the `zenha` desktop client |
| `cargo fmt --all` | Formats all workspace code |

To package a release build locally into `dist\`, run `scripts\package.cmd` (compiles the Inno Setup installer automatically if Inno Setup 6 is installed).

## Architecture

Zenha separates its `egui` desktop UI (`rooms-client`) from the LiveKit media engine (`rooms-core`) using non-blocking `Command` and `Event` channels: the UI thread never blocks or performs I/O, while `rooms-core` manages WebRTC/LiveKit sessions, audio processing, and screen capture on a Tokio runtime and dedicated capture threads. A stateless HTTP service (`rooms-token-server`) validates invite codes and issues short-lived, room-scoped LiveKit JWTs.

See [docs/architecture.md](docs/architecture.md) for component diagrams, media flows, and thread boundaries, and [docs/adr/](docs/adr/) for architecture decision records.

### Repository Layout

| Path | Contents |
| --- | --- |
| `crates/rooms-protocol` | Shared serde types and validation limits |
| `crates/rooms-core` | Session, audio, noise suppression, screen capture, and video; the only crate using the LiveKit SDK |
| `crates/rooms-client` | Desktop UI (`zenha`) built with `eframe`/`egui` |
| `crates/rooms-token-server` | HTTP server that validates invite codes and issues LiveKit tokens |
| `crates/rooms-e2e` | End-to-end test harness with scripted bots and WASAPI loopback probe |
| `xtask` | Workspace automation (`cargo ci`, icon generation) |
| `deploy` | Production server configuration (Docker Compose, Caddy, LiveKit) |
| `installer` | Inno Setup script for the Windows installer |
| `docs` | Architecture documentation and ADRs |

`CLAUDE.md` files in the repository root and crate directories serve as concise technical guides for AI coding agents working in the codebase.

## Contributing & Security

- Read [CONTRIBUTING.md](CONTRIBUTING.md) for coding conventions, crate boundaries, commit format, and manual Windows testing guidelines.
- Read [SECURITY.md](SECURITY.md) for instructions on reporting security vulnerabilities.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
