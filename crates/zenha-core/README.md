# zenha-core

The client engine: LiveKit session, token client, microphone, noise suppression, screen and share-audio capture, video and file transfers. It is the only crate that depends on `livekit` and `libwebrtc`, and no SDK type appears in its public API.

The UI sends `Command`s and reads `Event`s through `SessionHandle`; the session runs on its own Tokio runtime.

| Module | Contents |
| --- | --- |
| `api` | Public `Command`, `Event`, `SessionError` and related types |
| `handle` | `SessionHandle`: starts the runtime and owns the channels |
| `session/` | Session loop: `connection` (state machine, reconnect with backoff), `rooms`, `audio`, `chat`, `files`, `share`, `admin` |
| `backend`, `no_media` | `MediaBackend` trait and a backend without media |
| `livekit_backend/` | Real backend on the LiveKit SDK: `room`, `audio`, `events`, `data`, `stream_player` |
| `token` | `TokenClient` / `HttpTokenClient` for the token server |
| `mic_capture`, `share_audio`, `audio_render` | WASAPI capture, per-process loopback and output (Windows only) |
| `noise`, `deep_filter` | `Denoiser` trait, RNNoise, DeepFilterNet 3, noise gate |
| `capture`, `livekit_video`, `video` | `FrameSource` trait, desktop capturer, `FrameSlot` and share presets |
| `files`, `file_receive`, `livekit_files` | File naming, safe writes and LiveKit byte streams |
| `testing` | Test doubles (behind `cfg(test)` or the `test-util` feature) |

Windows-only code sits behind `#[cfg(windows)]`. `unsafe` is allowed only here, with `#[expect(unsafe_code)]` and a `SAFETY:` comment.

```powershell
cargo test -p zenha-core
```

The tests use `NoMediaBackend` and fake token clients, so they need no `livekit-server`. Audio, capture and real connections need manual testing on Windows or [`zenha-e2e`](../zenha-e2e/README.md).
