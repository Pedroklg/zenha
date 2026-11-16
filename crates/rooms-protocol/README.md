# rooms-protocol

JSON types shared by the client (`rooms-core`) and `rooms-token-server`: token and room list requests, saved chat, admin requests and the data packets the server sends to rooms (`admin.stop_share`, `admin.move`, `admin.chat_delete`, `rooms.changed`). It also holds the `MAX_*` limits and the `validate_*` functions both sides use.

Only `serde`; no I/O, no async. Everything is in [`src/lib.rs`](src/lib.rs).

A field change breaks client and server together, so change both in the same commit. Each new type gets a JSON round-trip test.

```powershell
cargo test -p rooms-protocol
```
