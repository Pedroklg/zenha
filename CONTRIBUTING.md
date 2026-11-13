# Convenções

## Código

- `cargo ci` precisa passar antes de cada commit.
- Sem `unwrap`, `expect` ou `panic!` fora de testes. Libs devolvem erros com `thiserror`; binários usam `anyhow` com contexto.
- `unsafe` é proibido por lint. Exceções só em `rooms-core`, com `#[expect(unsafe_code)]` e um comentário `SAFETY:`.
- Comentários só onde o motivo não é óbvio pelo código.
- Identificadores e mensagens de log em inglês; documentação e texto da interface em português.
- Dependências novas entram em `[workspace.dependencies]` do `Cargo.toml` raiz e são referenciadas com `workspace = true`.
- As crates `livekit`, `livekit-api` e `libwebrtc` usam versão exata (`=x.y.z`) e são atualizadas de propósito, uma por vez.

## Limites entre crates

- `rooms-protocol` não faz I/O.
- Só `rooms-core` importa `livekit` e `libwebrtc`. Nenhum tipo do SDK aparece na API pública dela.
- `rooms-client` fala com `rooms-core` por `Command` e `Event`; a thread da interface nunca bloqueia.

## Git

- Branch `main` sempre compilando; trabalho em branches curtas `tNN-descricao`.
- Commits no padrão Conventional Commits: `feat(core): ...`, `fix(client): ...`, `docs: ...`, `chore: ...`.
- Um commit por mudança lógica; a task do plano vai no corpo (`Refs: T06`).

## Segredos

- `.env` e arquivos `*.tfvars` nunca entram no git e não devem ser lidos por agentes.
- Variável nova entra em `.env.example` com um valor de exemplo.

## Decisões

Decisões de arquitetura viram um ADR curto em `docs/adr/`, a partir de `0000-template.md`.
