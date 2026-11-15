# Guia de code review do Zenha

Checklist para revisar uma branch, commit ou worktree de agente contra os padrões de `docs/standards/`. Formato dos achados e severidades em [README.md](README.md).

## Antes de começar

1. Rode as verificações automáticas em modo somente leitura (sem `--fix` e sem reescrever arquivos):
   - `cargo review` (`cargo xtask review-checks`)
   - `cargo clippy --workspace --all-targets -- -D warnings`
2. Leia o escopo da mudança (objetivo do commit/PR ou `AGY_BRIEF.md` quando for worktree de agente).
3. Inspecione o diff separado por crate/área (`rooms-core`, `rooms-client`, `rooms-protocol`, `rooms-token-server`, `rooms-e2e`, `deploy/`/`.github/`/`xtask`).

## Checklist de bloqueantes

Confira todos em toda revisão:

1. **Fronteira do SDK LiveKit (`CORE-01`, `UI-01`):** alguma crate fora de `rooms-core` importou `livekit` ou `libwebrtc`, ou item `pub` de `rooms-core` expôs tipo do SDK?
2. **Bloqueio na thread da UI (`UI-02`):** `fn ui` (ou função chamada por ela em `views/`, `state.rs` ou `app.rs`) faz rede, disco, `keyring` síncrono, `recv()` bloqueante ou `std::thread::sleep`?
3. **Bloqueio ou deadlock no Tokio (`CORE-06`, `SRV-05`, `SRV-02`):** existe `std::sync::MutexGuard` ou `RwLock*Guard` vivo através de `.await`, ou I/O síncrono (`rusqlite`, arquivo, COM, `thread::sleep`) direto em `async fn` sem `spawn_blocking`?
4. **Pânico em produção (`GER-01`, `CORE-08`, `GER-09`):** entrou `unwrap()`, `expect()`, `panic!()`, `todo!()`, `.lock().unwrap()` fora de `#[cfg(test)]`, ou indexação `slice[i]` sem checagem de limites (`BgraFrame::is_consistent`)?
5. **Memória e FFI (`GER-03`, `CORE-03`, `CORE-04`):** entrou `unsafe` fora de `rooms-core`, ou em `rooms-core` sem `#[expect(unsafe_code, reason = "...")]` e `// SAFETY:` provando a invariante? Thread WASAPI inicializa e encerra COM (`initialize_mta()` / `deinitialize()`) em todos os caminhos de saída?
6. **Cancelamento e vazamento de recurso (`CORE-07`, `CORE-10`):** thread/tarefa criada sem parada no `Drop` (`AtomicBool` + `join`)? Ramo de `tokio::select!` descarta future não cancelável perdendo estado? Arquivo `.part` de transferência fica em disco ao falhar?
7. **Contrato e pontos de ligação (`PROTO-02`, `PROTO-03`, `CORE-12`):** mudança em `rooms-protocol` desincronizou cliente e servidor? Novo `Command`/`Event` ou método em `MediaBackend` esqueceu algum dos 8 pontos de ligação (`api.rs`, `backend.rs`, `session.rs`, `testing.rs`, `no_media.rs`, `livekit_backend.rs`, `rooms-client`, `rooms-e2e/src/protocol.rs`)?
8. **Autenticação, autorização e validação no servidor (`SRV-02`, `SRV-03`, `SRV-04`, `PROTO-02`):** rota de chat confiou em `sender` do body em vez do JWT Bearer (`authenticate_room`)? Token emitido sem validar sala/identidade ou com escopo além da sala? Comparação de segredo fora de Argon2/`ConstantTimeEq`?
9. **Segredos e dados sensíveis (`GER-06`, `SRV-01`, `OPS-06`):** senha, `invite_code`, `LIVEKIT_API_SECRET` ou JWT apareceu em `tracing::*!`, resposta de erro HTTP, `#[derive(Debug)]` ou arquivo rastreado pelo git?
10. **Regressão em refactor e testes (`GER-10`, `TST-01`, `TST-04`, `TST-06`):** sumiu validação, filtro, rate limit ou teste existente? Lógica pura nova veio sem teste, ou teste confirma o comportamento errado?

## Improvements e observations frequentes

- **Improvements:**
  - Alocação por quadro/pacote em laço quente de áudio (48 kHz) ou vídeo (30–60 fps) (`CORE-09`) ou fila sem descarte de atraso (`CORE-05`).
  - Erro de lib usando `anyhow` em vez de `thiserror`, ou binário sem `.context(...)` (`GER-02`).
  - Idioma trocado: identificador/log em português, UI ou `//!` em inglês (`GER-04`).
  - Estado mutado direto em `views/*.rs` em vez de `UserAction` -> `AppState` (`UI-03`), ou `--demo` (`DemoSession`) quebrado (`UI-09`).
  - Handler HTTP novo sem teste `tower::ServiceExt::oneshot` (`TST-02`), tipo novo em `rooms-protocol` sem round-trip JSON (`PROTO-04`), ou `std::env::var` fora de `from_lookup` (`SRV-08`, `TST-05`).
  - Cast `as` com risco de truncamento onde cabia `try_from` ou `.saturating_*` (`GER-09`).
- **Observations:** nomes genéricos (`GER-04`), comentários narrativos (`GER-05`), funções longas ou muito aninhadas que melhoram com early return (`GER-12`).

## Divisão entre máquina e revisor

Não repita no review o que `cargo fmt`, `cargo clippy` ou `cargo review` já acusaram na saída das ferramentas.

O `cargo ci` roda as checagens de fronteira (`CORE-01`, `PROTO-01`, `GER-03`, `GER-07`, `OPS-06`); o `cargo review` roda essas e as demais da tabela. `GER-04`, `TST-04` e `CORE-12` saem como `warning` (heurística); o resto é `error`.

| Regra | O que a máquina já pega | O que o revisor confere no código |
|---|---|---|
| `GER-01`, `GER-08` | Clippy `unwrap_used`, `expect_used`, `panic`, `todo`, `dbg_macro`, `allow_attributes`, `allow_attributes_without_reason` | Indexação `slice[i]` sem bounds check, `unreachable!()` injustificado, validade do `reason` |
| `GER-02` | Clippy `unused_must_use` | `let _ = ...` engolindo erro relevante, `anyhow` em libs, falta de `.context(...)` em binários |
| `GER-03`, `CORE-03` | Rustc `unsafe_code = "deny"`, Clippy `undocumented_unsafe_blocks`, `xtask review-checks` | Se `unsafe` é realmente necessário e se o `// SAFETY:` prova a invariante |
| `GER-04` | `xtask review-checks`: `tracing::*!` com inicial maiúscula | Idioma dos identificadores, docs `//!`/`///` em PT-BR e strings da UI em PT-BR |
| `GER-06`, `SRV-01` | `xtask review-checks`: `#[derive(... Debug ...)]` em struct com `api_secret`, `livekit_api_secret`, `invite_code_hash`, `admin_code_hash`, `password_hash` ou `private_key` | Segredo interpolado em logs ou respostas HTTP, e `Debug` em tipos novos com credenciais |
| `GER-07` | `cargo deny check bans` (`cargo ci`), `xtask review-checks`: versão inline fora da raiz e versão `=` nas deps críticas | Se dependência nova é necessária e usa `default-features = false` |
| `GER-09`, `GER-10`, `GER-11`, `GER-12` | Clippy `large_futures`, `mutex_atomic`, `rc_buffer`, `rc_mutex` e demais lints do workspace | Truncamento em `as`, regras/limites removidos em refactor, constantes nomeadas e extração de lógica |
| `CORE-01`, `UI-01` | `cargo deny` (`wrappers`), `xtask review-checks`: imports/paths de `livekit`/`libwebrtc` fora de `rooms-core` e na API `pub` | Vazamento indireto de semântica do SDK na API pública ou acoplamento interno na UI |
| `CORE-12` | `xtask review-checks`: alerta quando `enum Command`/`Event` muda sem tocar os 8 pontos de ligação | Tratamento completo e coerente nos 8 pontos |
| `PROTO-01` | `xtask review-checks`: dependências e I/O/async em `rooms-protocol` | Validação nas duas pontas (`PROTO-02`), compatibilidade JSON (`PROTO-03`) e `.chars().count()` (`PROTO-05`) |
| `SRV-05`, `TST-04`, `TST-05`, `OPS-03`, `OPS-06` | `xtask review-checks`: `format!` em SQL no `chat_store.rs`, `#[ignore]` sem justificativa, `:latest` em `deploy/`, arquivos proibidos rastreados pelo git; Clippy `disallowed-methods` (`env::set_var`) | Demais regras de `SRV-*`, `TST-*` e `OPS-*` |

## Não comentar

- Formatação e lints que já saíram em `cargo clippy` ou `cargo review`.
- Código legado em arquivos grandes que o diff não tocou nem agravou.
- Preferência estilística pessoal fora das regras de `docs/standards/`.
