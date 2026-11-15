# Regras de `rooms-token-server`: segurança, autenticação e SQLite (SRV)

Complementa [general.md](general.md) e [protocol.md](protocol.md). Escopo: `crates/rooms-token-server/`.

## SRV-01: Segredos nunca aparecem em logs nem em respostas HTTP

- `Config` implementa `fmt::Debug` manualmente (`crates/rooms-token-server/src/config.rs`), omitindo `invite_code`, `invite_code_hash`, `admin_code`, `admin_code_hash`, `livekit_api_key` e `livekit_api_secret`.
- `ApiError` (`crates/rooms-token-server/src/routes.rs`) retorna apenas mensagens fixas e controladas; erros internos do Argon2, LiveKit ou SQLite ficam só no log (`tracing::error!` / `tracing::warn!`) sem credenciais nem payloads sensíveis.

## SRV-02: Verificação de código de convite/admin com Argon2 ou tempo constante

- Quando `ROOMS_INVITE_CODE_HASH` está configurado, o servidor valida o formato PHC na partida e verifica tentativas com `Argon2::default().verify_password(...)`.
- No fallback em texto (`ROOMS_INVITE_CODE`), emite aviso `tracing::warn!` na partida e compara os bytes em tempo constante (`subtle::ConstantTimeEq`), sem vazar tamanho pelo tempo de resposta.
- **Atenção em review (CPU-bound no Tokio):** `Argon2::verify_password` consome CPU (~15–50 ms por chamada); em rotas sujeitas a rajadas, avalie executar em `tokio::task::spawn_blocking` ou aplicar rate limit.

## SRV-03: Emissão de JWT com escopo mínimo e identidade higienizada

- O token emitido em `token::mint` (`crates/rooms-token-server/src/token.rs`) usa `identity = client_id` (validado), `name = display_name.trim()`, `VideoGrants` restrito à sala pedida (`room_join: true`, `room: room.to_owned()`, sem `room_admin` irrestrito) e `ttl` validado (`1..=MAX_TOKEN_TTL_SECS`).

## SRV-04: Rotas de chat autenticadas pelo Bearer JWT; autor vem do token, nunca do body

- Rotas em `/rooms/{room}/chat` autenticam via `authenticate_room` (`routes.rs`): exigem `Authorization: Bearer <token>`, validam assinatura/expiração, conferem `claims.video.room_join` e `claims.video.room == room` (`403 Forbidden` se divergir) e extraem `sender_id` (`claims.sub`) e `sender_name` (`claims.name`) do token — nunca do body.
- Na exclusão (`DELETE`), `chat_store.rs` exige `sender_id` coincidente com o dono da mensagem antes de apagar (`403`).

## SRV-05: Acesso ao SQLite via `spawn_blocking`, queries parametrizadas e `Immediate`

- Todo acesso ao `rusqlite::Connection` em `ChatStore` (`chat_store.rs`) roda dentro de `tokio::task::spawn_blocking` via `self.with_conn(...)`, nunca bloqueando a thread worker do Tokio.
- A conexão usa `busy_timeout`, `journal_mode = WAL` e `synchronous = NORMAL`.
- Toda query SQL usa placeholders posicionais (`?1`, `?2`, `params![...]`), nunca interpolação `format!`.
- Escritas que leem e modificam em sequência usam `TransactionBehavior::Immediate`, e `sent_at_unix_ms` é limitado por `clamp_sent_at`.

## SRV-06: Rate limiting com limite de memória

- `ChatRateLimiter` (`routes.rs`) limita mensagens por janela e poda entradas expiradas ao atingir o limite de entradas em memória, evitando crescimento ilimitado.
- Qualquer novo limitador ou cache em memória deve ter teto de chaves e poda determinística testada com tempo injetado (`check_at`).

## SRV-07: Subcomando `hash-password` isolado

- `rooms-token-server hash-password` lê a senha do `stdin`, valida não vazia e imprime o hash PHC Argon2id sem carregar `.env`, exigir variáveis do servidor ou abrir socket de rede.

## SRV-08: Variáveis de ambiente documentadas e testadas sem tocar o ambiente global

- `Config::from_env` delega para `Config::from_lookup(impl Fn(&str) -> Option<String>)` (`config.rs`), permitindo testar sem `std::env::set_var` (proibido pelo `clippy.toml`).
- Toda variável de ambiente nova deve ser documentada com valor de exemplo seguro em `.env.example`.
