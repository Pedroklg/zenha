# 0002. egui with wgpu for the UI

- Status: accepted
- Date: 2026-11-09

## Context

Media runs in native Rust and decoded video frames must be displayed in the window. Windows is the only target.

## Decision

Use `eframe`/`egui` with `wgpu`, rendering video frames as a texture.

## Alternatives Considered

- **Tauri with React and Rust media.** Rendering native video requires either a synchronized child window under the webview or pushing frames into the webview, an approach without public 1080p60 benchmarks.
- **Tauri or Electron with the JavaScript SDK.** Lower effort, but moves media outside Rust.
- **iced.** Cleaner architecture, but the video widget would have to be written from scratch.
- **GPUI.** Sparse documentation and Windows outside the README's supported platforms list.

## Consequences

- An official reference client exists with the same stack (`rust-dev-client`).
- Visual styling is simpler than a web-based UI.
- `egui` is pre-1.0 and changes its API between releases; the version stays pinned.
