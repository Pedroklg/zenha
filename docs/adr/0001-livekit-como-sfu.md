# 0001. LiveKit as SFU, with Client on the Rust SDK

- Status: accepted
- Date: 2026-11-09

## Context

The app targets a small group with voice and screen sharing. In a P2P mesh, the broadcaster encodes and uploads video once per viewer, which does not fit typical home upload bandwidth. The goal is to write all custom code in Rust.

## Decision

Use `livekit-server` as a prebuilt binary and the Rust SDK (`livekit`, `libwebrtc`) in the client, isolated inside the `rooms-core` crate.

## Alternatives Considered

- **Custom SFU with `str0m`.** `str0m` only covers transport; codecs, jitter buffer, echo cancellation, and capture would all have to be built by the project.
- **Prebuilt SFU written in Rust.** As of November 2026 none is mature with a native Rust client (`webrtc-rs/sfu` is pre-production, `rheomesh` only has a TypeScript client, `atm0s-media-server` is stalled).
- **MoQ or iroh-live.** Technical previews; `iroh-live` had not been run on Windows by its maintainers.
- **JavaScript SDK inside a webview.** Faster to build, but moves media outside Rust.

## Consequences

- The server is written in Go, but it is only operated, never modified.
- The client speaks the LiveKit protocol; switching SFUs requires rewriting the networking layer of `rooms-core`.
- The SDK is 0.x and breaks its API frequently: versions are pinned exactly and updated deliberately.
