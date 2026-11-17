# zenha-server

`axum` HTTP server. It checks the invite code (Argon2), issues short-lived LiveKit tokens for one room, lists rooms with their occupants, checks per-room passwords, stores saved chat messages and rooms in SQLite, and serves the optional admin routes. See the [join flow](../../docs/architecture.md#joining-a-room).

| Module | Contents |
| --- | --- |
| `main` | Startup and the `hash-password` subcommand |
| `config` | `Config` from environment variables (secrets omitted from `Debug`) |
| `routes/` | Router and auth (`mod`), `token`, `rooms`, `chat`, `admin`, `rate_limit` |
| `token` | LiveKit JWT signing |
| `directory` | LiveKit server API: participants, moderation, data packets |
| `store/` | SQLite store for saved messages, rooms (title, order, password hash, parent) and the rotated invite hash |
| `room_access` | Room keys (HMAC-SHA256) and the rule that a parent's password opens its children |

Main routes: `GET /health`, `POST /token`, `GET /rooms`, `POST /rooms/{room}/unlock`, `GET`/`POST /rooms/{room}/chat`, `DELETE /rooms/{room}/chat/{id}`, `PUT`/`DELETE /avatar`, `GET /avatar/{identity}` and `/admin/*` (only when `ZENHA_ADMIN_CODE_HASH` is set).

Room passwords: an admin sets `password` and `parent` in `POST /admin/rooms` and `PATCH /admin/rooms/{id}` (an empty string removes them; a parent must be a top-level room). `POST /rooms/{room}/unlock` takes `{invite_code, password}` and returns `{room, room_key}`, where `room` is the room that owns the password (the room itself or its parent). Clients send keys in the `x-room-keys: room=key,...` header of `GET /rooms` and in `room_key` of `POST /token`; a locked room without a valid key lists no occupants and its token request fails with `403 room_locked`. Admin moves ignore room passwords.

Configuration comes from environment variables (`.env` in development; see [.env.example](../../.env.example)): `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `ZENHA_INVITE_CODE_HASH` (or plaintext `ZENHA_INVITE_CODE`, with a startup warning), `ZENHA_ADMIN_CODE_HASH`, `ZENHA_BIND_ADDR`, `ZENHA_CHAT_DB`, `ZENHA_ROOM_LIST`, `ZENHA_TOKEN_TTL_SECS`.

```powershell
cargo server                                             # run with .env
"password" | cargo run -p zenha-server -- hash-password
cargo test -p zenha-server
```

Handler tests call the router with `tower::ServiceExt::oneshot`, an in-memory SQLite store and a fake LiveKit directory; no port or `livekit-server` is needed. Production deployment: [docs/deploy.md](../../docs/deploy.md).
