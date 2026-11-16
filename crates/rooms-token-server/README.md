# rooms-token-server

`axum` HTTP server. It checks the invite code (Argon2), issues short-lived LiveKit tokens for one room, lists rooms with their occupants, stores saved chat messages and rooms in SQLite, and serves the optional admin routes. See the [join flow](../../docs/architecture.md#joining-a-room).

| Module | Contents |
| --- | --- |
| `main` | Startup and the `hash-password` subcommand |
| `config` | `Config` from environment variables (secrets omitted from `Debug`) |
| `routes/` | Router and auth (`mod`), `token`, `rooms`, `chat`, `admin`, `rate_limit` |
| `token` | LiveKit JWT signing |
| `directory` | LiveKit server API: participants, moderation, data packets |
| `chat_store` | SQLite store for saved messages, rooms and the rotated invite hash |

Main routes: `GET /health`, `POST /token`, `GET /rooms`, `GET`/`POST /rooms/{room}/chat`, `DELETE /rooms/{room}/chat/{id}` and `/admin/*` (only when `ROOMS_ADMIN_CODE_HASH` is set).

Configuration comes from environment variables (`.env` in development; see [.env.example](../../.env.example)): `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `ROOMS_INVITE_CODE_HASH` (or plaintext `ROOMS_INVITE_CODE`, with a startup warning), `ROOMS_ADMIN_CODE_HASH`, `ROOMS_BIND_ADDR`, `ROOMS_CHAT_DB`, `ROOMS_ROOM_LIST`, `ROOMS_TOKEN_TTL_SECS`.

```powershell
cargo server                                             # run with .env
"password" | cargo run -p rooms-token-server -- hash-password
cargo test -p rooms-token-server
```

Handler tests call the router with `tower::ServiceExt::oneshot`, an in-memory SQLite store and a fake LiveKit directory; no port or `livekit-server` is needed. Production deployment: [docs/deploy.md](../../docs/deploy.md).
