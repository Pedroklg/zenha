# Regras gerais de Rust (GER)

Valem para todas as crates do workspace (`crates/*` e `xtask`).

## GER-01: Sem `unwrap`, `expect`, `panic!`, `todo!` ou `dbg!` fora de testes

- Código de produção propaga erros com `?`, trata com `if let` / `let ... else` ou faz fallback seguro com `unwrap_or` / `unwrap_or_else`.
- Em `#[cfg(test)]`, `unwrap()`, `expect(...)`, `panic!(...)` e `dbg!(...)` são permitidos via `clippy.toml`.
- **Por quê:** pânico em thread de captura, sessão ou UI derruba a chamada de voz ou fecha o cliente sem recuperação.

## GER-02: Tratamento de erros (`thiserror` em libs, `anyhow` em binários)

- Crates de biblioteca (`rooms-protocol`, `rooms-core`) definem erros tipados com `#[derive(Debug, thiserror::Error)]` (`ValidationError`, `SessionError`, `ChatStoreError`).
- Binários (`rooms-client`, `rooms-token-server`, `rooms-e2e`) podem usar `anyhow::Result` na borda de inicialização/CLI, sempre com `.context("...")` ou `.with_context(|| ...)` em I/O.
- Não engula `Result` relevante com `let _ = ...`; descartes intencionais (como canal fechado no desligamento ou `request_repaint`) devem estar óbvios no contexto.

## GER-03: `unsafe` proibido fora de `rooms-core`

- O workspace define `unsafe_code = "deny"` em `[workspace.lints.rust]`.
- Nenhuma crate fora de `crates/rooms-core` pode conter blocos, funções, traits ou impls `unsafe`.
- Dentro de `rooms-core`, toda ocorrência exige `#[expect(unsafe_code, reason = "...")]` (nunca `#[allow(unsafe_code)]`) junto de um comentário `// SAFETY:` logo acima do bloco, explicando a invariante garantida (veja `CORE-03` e `crates/rooms-core/src/share_audio.rs`).

## GER-04: Idioma de código, logs, docs e interface

- Identificadores (tipos, funções, variáveis, módulos, variantes) sempre em inglês.
- Logs (`tracing::info!`, `warn!`, `error!`, `debug!`, `trace!`) em inglês, começando em letra minúscula, com campos estruturados antes da mensagem (ex.: `tracing::warn!(room = %name, %err, "room lookup failed")`).
- Doc-comments (`//!`, `///`) em português do Brasil; comentários de linha internos (`//`) em inglês.
- Textos visíveis ao usuário na interface (`rooms-client`) e nos relatórios do `rooms-e2e` em português do Brasil.

## GER-05: Comentários mínimos e só quando o motivo não é óbvio

- Sem comentários narrativos que repetem o que a linha faz.
- Comente apenas invariantes não triviais, restrições de API do Windows/LiveKit, limiares de DSP/rede ou blocos `SAFETY:`.
- Comentário desatualizado em relação ao código é bug e deve ser corrigido ou removido.

## GER-06: Segredos nunca em logs, erros ou `Debug` derivado

- Tipos que guardam senhas, códigos de convite, tokens JWT ou chaves da API do LiveKit nunca usam `#[derive(Debug)]`: implementam `fmt::Debug` manualmente omitindo os campos sensíveis (`"<redacted>"` ou `finish_non_exhaustive()`), como em `SessionConfig` (`crates/rooms-core/src/api.rs`) e `Config` (`crates/rooms-token-server/src/config.rs`).
- Nenhum `tracing::*!` ou erro HTTP/UI pode interpolar `invite_code`, `livekit_api_secret`, `token` ou `password`.

## GER-07: Dependências centralizadas em `[workspace.dependencies]`

- Toda dependência externa fica declarada em `[workspace.dependencies]` no `Cargo.toml` da raiz (preferencialmente com `default-features = false`) e é referenciada nas crates com `<dep>.workspace = true`.
- `livekit`, `livekit-api`, `argon2`, `global-hotkey`, `keyring`, `tray-icon`, `wasapi` e `winresource` mantêm versão exata (`=x.y.z`); `libwebrtc` fica fixada no `[patch.crates-io]`.
- Nenhuma crate declara `version = "..."` inline no próprio `Cargo.toml`.

## GER-08: Supressão de lint só com `#[expect(..., reason = "...")]`

- Prefira corrigir o aviso do compilador ou do Clippy.
- Quando a supressão for necessária, use `#[expect(lint_name, reason = "...")]` no menor escopo possível, nunca `#![allow(...)]` nem supressões sem `reason`.
- `#[expect]` avisa automaticamente quando o lint deixa de disparar, evitando atributos mortos.

## GER-09: Conversões numéricas explícitas e aritmética segura

- Evite `as` entre inteiros quando houver risco de truncamento ou mudança de sinal (`u64` para `usize`/`u32`, `i64` para `u64`, `f32` para `i16`): prefira `try_from` com fallback explícito (`unwrap_or`) ou `.clamp(...)` antes do cast.
- Em cálculos de tempo, índices de buffer de áudio/vídeo e contadores, use `saturating_add`, `saturating_sub`, `saturating_mul` ou `saturating_duration_since`.

## GER-10: Refactor preserva comportamento e limpa código morto nas duas pontas

- Regra de validação, timeout, limpeza em `Drop`, log de erro ou checagem de permissão removida em refactor é bug até prova em contrário.
- Ao alterar ou remover uma variante de `Command`, `Event` ou rota HTTP, remova também tipos, handlers, mocks e traduções órfãs em todas as crates.

## GER-11: Constantes nomeadas para limites, tempos e tamanhos

- Timeouts, taxas de amostragem (`48_000`), tamanhos de quadro (`480`), limites de backlog, intervalos de repaint e portas padrão ficam em `const` em `SCREAMING_SNAKE_CASE` no topo do módulo (ou em `rooms-protocol` quando compartilhados).

## GER-12: Tamanho e clareza de funções e módulos

- Funções novas devem manter baixa complexidade e fluxo linear com `early return` (`let Some(...) = ... else { return ... };`) em vez de `if`/`match` muito aninhados.
- Ao acrescentar lógica nova independente a módulos extensos (`state.rs`, `session.rs`, `livekit_backend.rs`, `routes.rs`), extraia submódulos ou helpers coesos em vez de inflar o bloco monolítico.
