# voice-rooms

App desktop Windows em Rust: salas de voz, chat de texto e transmissão de tela para um grupo pequeno de amigos. SFU: `livekit-server` auto-hospedado (binário pronto). Todo o código próprio é Rust.

Plano e tasks: `docs/plan.md`. Arquitetura: `docs/architecture.md`. Decisões: `docs/adr/`.

## Comandos

- `cargo ci`: fmt + clippy (`-D warnings`) + testes. Rode antes de dar uma task por concluída.
- `cargo lint`, `cargo server`, `cargo client`, `cargo fmt --all`.
- Alvo real: `x86_64-pc-windows-msvc`. Compile nativo no Windows, não no WSL.

## Regras de arquitetura

- Só `crates/rooms-core` pode depender de `livekit` e `libwebrtc`. Tipos do SDK não aparecem na API pública dela.
- `rooms-client` nunca importa `livekit`; conversa com `rooms-core` por `Command` (ida) e `Event` (volta), via canais.
- A thread da interface nunca bloqueia nem faz I/O. Rede e sessão rodam no runtime Tokio da `rooms-core`.
- `rooms-protocol` só tem tipos `serde`, sem I/O e sem async.
- Captura de tela fica atrás do trait `FrameSource`, para trocar o capturador sem mexer no resto.

## Regras de código

- Sem `unwrap`, `expect`, `panic!`, `todo!` ou `dbg!` fora de testes (lint).
- `unsafe` negado por lint; exceção só em `rooms-core`, com `#[expect(unsafe_code)]` e comentário `SAFETY:`.
- Erros: `thiserror` em libs, `anyhow` com `.context(...)` em binários.
- Código sem comentários; no máximo um comentário curto em trecho não trivial.
- Identificadores e logs em inglês; docs e texto de interface em português.
- Segredos nunca em log: `Config` tem `Debug` manual que os omite.
- Dependências em `[workspace.dependencies]`; `livekit`, `livekit-api` e `libwebrtc` com versão exata.

## Versões de referência (crates.io, 2026-11-09)

`livekit` 0.10.1, `livekit-api` 0.10.1, `libwebrtc` 0.4.0, `eframe` 0.37.0, `wasapi` 0.27.0, `global-hotkey` 0.10.0, `windows-capture` 2.0.1.

O SDK do LiveKit muda a API quase toda semana. Antes de usar uma função dele, confira a assinatura no código da versão fixada (`~/.cargo/registry/src/`) ou nos exemplos do repositório `livekit/rust-sdks`; não confie na memória.

`eframe` 0.37: `impl eframe::App` exige `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)`, não mais `update`.

## Arquivos proibidos

- Respeite o `.gitignore`: não leia nem edite arquivos ignorados.
- Nunca leia `.env`, `.env.*`, `*.tfvars`, `*.pem`, `*.key` ou `deploy/livekit.prod.yaml`. Se precisar de um valor deles, peça ao usuário, que envia anonimizado.

## Fluxo de trabalho

1. Pegue a próxima task em `docs/plan.md` e leia o critério "Pronto quando".
2. Implemente só o escopo da task; mudanças fora dele viram nota para o usuário.
3. Escreva testes para lógica sem dependência de hardware (protocolo, config, estado).
4. Rode `cargo ci`.
5. Commit no padrão Conventional Commits, com `Refs: TNN`. Só faça commit ou push quando o usuário pedir.
6. Marque a task em `docs/plan.md`.

O que não dá para testar automaticamente (áudio, captura, GPU) precisa de teste manual no Windows; diga isso com clareza em vez de afirmar que funciona.
