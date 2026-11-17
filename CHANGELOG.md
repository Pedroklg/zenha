# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Windows desktop client (`zenha`) built with `eframe`/`egui` (`wgpu`), dark theme, embedded Inter and Space Grotesk fonts, and Brazilian Portuguese UI.
- First-access onboarding screen, `zenha://` invite link parsing, room lobby, and Windows Credential Manager (`keyring`) password storage.
- Voice rooms powered by `zenha-core` and LiveKit Rust SDK (`livekit`/`libwebrtc`) with active-speaker indicators, mute/deafen, audio device selection, and automatic reconnection with backoff.
- Multi-tier microphone noise suppression: Off, WebRTC Default, Enhanced (RNNoise via `nnnoiseless` over WASAPI capture), and Maximum (DeepFilterNet 3 with automatic fallback to RNNoise), plus an adjustable noise gate and live microphone level meter.
- Global hotkeys (`global-hotkey`) for mute, deafen, and push-to-talk (with release delay and synthesized audio feedback cues).
- System tray integration (`tray-icon`) with close-to-tray call persistence and tray menu actions.
- Screen and window sharing (`DesktopCapturer`, VP9, 720p30 / 1080p30 / experimental 1080p60 presets, fullscreen viewer) with per-process or system WASAPI loopback audio capture and an independent viewer volume slider.
- Reliable text chat with unread message counter and file/image sharing up to 50 MB (image thumbnails, transfer progress, and Save dialog).
- Stateless HTTP token server (`zenha-server`) with Argon2 invite code hashing (`hash-password` subcommand), room directory (`GET /rooms`), short-lived room-scoped LiveKit JWTs (`POST /token`), and per-installation `client_id`.
- End-to-end test harness (`zenha-e2e`) with scripted bots and WASAPI loopback audio detection.
- Windows Inno Setup installer (registering `zenha://`), portable `.zip` packaging, and GitHub Actions CI and Release workflows.
- Open-source documentation and repository setup: dual MIT / Apache-2.0 license, `README.md`, `CONTRIBUTING.md`, `SECURITY.md`, issue/PR templates, Dependabot, and `cargo-deny`.
