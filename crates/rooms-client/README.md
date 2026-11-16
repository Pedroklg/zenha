# rooms-client

The desktop app, built as `zenha.exe` with `eframe`/`egui`. It never imports `livekit`: it talks to [`rooms-core`](../rooms-core/README.md) through `Command` and `Event` channels and never blocks the UI thread.

| Module | Contents |
| --- | --- |
| `app` | `eframe::App`: reads pending events and draws each frame |
| `state/` | UI state and screen transitions: `onboarding`, `room`, `audio`, `chat`, `share`, `admin`, `update` |
| `views/` | Screens and panels: `onboarding`, `room`, `sidebar`, `chat`, `share`, `room_watch`, `participant_menu`, `settings` |
| `core_session`, `ui_model` | Bridge to `rooms-core` and the UI-side copies of its types |
| `demo` | `--demo` mode: simulated rooms and participants |
| `settings`, `credentials` | Settings in `%APPDATA%\zenha`, passwords in Windows Credential Manager |
| `hotkeys`, `global_keys`, `tray` | Global hotkeys and the tray icon (Windows) |
| `invite`, `cli`, `single_instance` | `zenha://` links and startup arguments |
| `update` | Checks GitHub Releases for a newer version |
| `theme`, `icons`, `chat_images`, `clipboard_image` | Look and feel, chat image thumbnails and pasted images |

UI text is Brazilian Portuguese.

```powershell
cargo client                # run against the token server in the saved settings
cargo client -- --demo      # no server needed
$env:ZENHA_CONFIG_DIR='C:\tmp\zenha-b'; cargo client   # second profile: own settings.json, runs beside the main instance
cargo test -p rooms-client
```

Tests cover state transitions and a UI smoke test with `egui_kittest`. Rendering, audio devices, hotkeys and the tray need manual testing on Windows.
