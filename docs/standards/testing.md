# Regras de testes: unitários, integração e `zenha-e2e` (TST)

Complementa [general.md](general.md). Vale para blocos `#[cfg(test)]` em todas as crates e para `crates/zenha-e2e/`.

## TST-01: Toda lógica pura nova ganha teste unitário

- Funções e máquinas de estado sem dependência direta de hardware (validação de protocolo, parsing de config/CLI/links, `AppState` da UI, `Pacer`, `FrameSlot`, `fit_within`, sanitização de caminhos, `ChatRateLimiter`, Goertzel e detecção de tom em `zenha-e2e`, checks do `xtask`) devem ter testes unitários no mesmo módulo.
- Correção de bug inclui teste que reproduz a falha antes do fix.

## TST-02: Handlers do `zenha-server` testados com `tower::ServiceExt::oneshot`

- Rotas e regras de autenticação/validação em `crates/zenha-server/src/routes/` são testadas montando o `Router` com `ChatStore::open_in_memory()` e `RoomDirectory` em memória, disparando requisições com `oneshot` sem abrir porta TCP.
- Cada caminho de erro (`400`, `401`, `403`, `404`, `409`, `429`, `502`, `503`) tem teste verificando o `StatusCode` e garantindo que o corpo JSON não vaza segredos.

## TST-03: Sessão e UI testadas com dublês determinísticos (`NoMediaBackend`, `TestPatternSource`, `DemoSession`)

- Testes de `session/` em `zenha-core` usam `NoMediaBackend` / dublês de `testing.rs` e um `TokenClient` fake, sem `livekit-server` real.
- Testes de captura sem GPU/SO usam `TestPatternSource` (`capture.rs`).
- Testes de UI exercitam `AppState` (`state/`) verificando transições e efeitos (`UiCommand`, `CredentialOp`).

## TST-04: Proibido apagar, enfraquecer ou ignorar testes existentes

- Nenhuma mudança pode remover teste existente, afrouxar asserção (`assert_eq!` trocado por `assert!(res.is_ok())`) ou adicionar `#[ignore]` sem justificativa explícita aprovada.
- Ajustes em `crates/zenha-e2e/src/detect.rs` exigem justificar limiares e manter os testes de `detect` e `dsp` passando.

## TST-05: Portabilidade e isolamento (sem `env::set_var`, sem rede externa, sem relógio solto)

- Testes unitários não dependem de hardware de áudio/vídeo, não acessam a internet e não mutam variáveis de ambiente (`std::env::set_var` / `remove_var` são proibidos em `clippy.toml`).
- Funções dependentes de relógio (`check_at`, `Pacer::new`, `clamp_sent_at`, `Throttle::ready`) recebem `Instant` ou timestamp por parâmetro.
- No `zenha-e2e`, o orquestrador nunca lê `.env` da raiz: sobe o `zenha-server` passando variáveis explícitas ao processo filho.

## TST-06: Asserções concretas nos caminhos de sucesso e de falha

- Prefira `assert_eq!(got, expected)` ou `assert!(matches!(...))` a `assert!(res.is_err())` genérico.
- Lógica dependente de múltiplos itens (paginação, deduplicação de dispositivos, poda do rate limiter, descarte de frames) deve ser testada com 2 ou mais elementos.

## TST-07: Declaração honesta do que exige teste manual no Windows

- Captura real do Windows (`DesktopCapturer`), WASAPI loopback/microfone, renderização `wgpu`, atalhos globais em tela cheia e Windows Credential Manager não são cobertos por `cargo test`.
- Alterações nessas áreas devem declarar explicitamente no relatório o que foi verificado pelos testes automatizados e qual roteiro manual no Windows (`cargo client -- --demo`, `scripts\dev-up.cmd`, `scripts\e2e.cmd`) falta validar.
