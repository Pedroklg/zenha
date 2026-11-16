# rooms-e2e

End-to-end test harness. `rooms-e2e run` starts the servers that are missing (`livekit-server --dev` from `tools\livekit\`, and `rooms-token-server`), launches headless bots as child processes, runs the scenarios and writes a report. `rooms-e2e bot` is one such bot: it reads JSON commands on stdin and writes JSON events on stdout. Bots use [`rooms-core`](../rooms-core/README.md), never `livekit` directly.

Audio is checked for real: bots publish a test tone and a WASAPI loopback probe on the output device detects it with a Goertzel filter. Video uses a test pattern.

| Module | Contents |
| --- | --- |
| `runner`, `scenarios`, `scenario` | Orchestrator, the scenarios and their results |
| `bot`, `bot_main`, `controller`, `protocol` | Bot process and the JSON line protocol |
| `dsp`, `detect`, `frames` | Tone analysis, detection thresholds, video frame checks |
| `probe`, `loopback` | Output probe; the WASAPI loopback is Windows only |
| `proc`, `cli`, `report` | Child processes, arguments, Markdown and JSON report |

```powershell
scripts\e2e.cmd                                       # build and run; report in dev-logs\e2e\report.md
cargo run -p rooms-e2e -- run --list                  # list scenarios
cargo run -p rooms-e2e -- run --filter name1,name2    # run some scenarios
cargo test -p rooms-e2e                               # pure logic only, no devices
```

`run` plays a quiet test tone on the output device. It never reads `.env`; it passes explicit variables to the token server.
