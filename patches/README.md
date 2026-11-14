# Patched dependencies

Local copies of two crates from [livekit/rust-sdks](https://github.com/livekit/rust-sdks),
wired in through `[patch.crates-io]` in the root `Cargo.toml`. Both are licensed under
Apache-2.0 by LiveKit, Inc.; see `webrtc-sys/NOTICE.md`.

| Crate | Version | Change |
| --- | --- | --- |
| `webrtc-sys` | 0.3.53 | Unmodified copy (see below) |
| `libwebrtc` | 0.4.0 | Unmodified copy (see below) |

When upgrading the LiveKit SDK, copy the new versions from the cargo registry and reapply the
changes listed here. Drop a crate from this folder once upstream ships the same API.
