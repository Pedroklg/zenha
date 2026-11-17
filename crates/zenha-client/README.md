# zenha-client

The desktop app, built as `zenha.exe` with `eframe`/`egui`. It never imports `livekit`: it talks to [`zenha-core`](../zenha-core/README.md) through `Command` and `Event` channels and never blocks the UI thread.

| Module | Contents |
| --- | --- |
| `app/` | `eframe::App`: `App` state and creation, frame loop, channel pumps, effects, window visibility |
| `state/` | UI state and screen transitions: `onboarding`, `room`, `audio`, `chat`, `share`, `admin`, `update`, plus `user_action`, `handle` (actions) and `events` |
| `views/` | Screens and panels: `onboarding`, `room/`, `sidebar/`, `chat`, `share`, `room_watch`, `participant_menu`, `settings` |
| `core_session/`, `ui_model` | Bridge to `zenha-core` (commands, event translation, errors, video feed) and the UI-side copies of its types |
| `demo/` | `--demo` mode: simulated rooms, participants, chat and sharing |
| `platform/` | System integration: `settings`, `credentials` (Windows Credential Manager), `hotkeys`, `global_keys`, `tray`, `single_instance`, `clipboard_image` |
| `updater/` | Update check on GitHub Releases, verified download and installer launch |
| `invite`, `cli` | `zenha://` links and startup arguments |
| `theme`, `icons`, `chat_images`, `avatars`, `dialogs`, `format` | Look and feel, chat image thumbnails, avatars, file dialogs and formatting helpers |

UI text is Brazilian Portuguese.

```powershell
cargo client                # run against the token server in the saved settings
cargo client -- --demo      # no server needed
$env:ZENHA_CONFIG_DIR='C:\tmp\zenha-b'; cargo client   # second profile: own settings.json, runs beside the main instance
cargo test -p zenha-client
```

Tests cover state transitions and a UI smoke test with `egui_kittest`. Rendering, audio devices, hotkeys and the tray need manual testing on Windows.
