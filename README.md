# voice-rooms

App desktop para Windows, em Rust, com salas de voz, chat de texto e transmissão de tela para um grupo pequeno de amigos. O servidor de mídia é um [LiveKit](https://github.com/livekit/livekit) auto-hospedado; todo o código deste repositório é Rust.

O plano completo está em [docs/plan.md](docs/plan.md) e o estado atual em [docs/status.md](docs/status.md).

## Requisitos

- Windows 10 ou 11, x64.
- [rustup](https://rustup.rs). A versão do Rust vem de `rust-toolchain.toml` e é instalada sozinha.
- Visual Studio Build Tools com a carga "Desktop development with C++".
- Binário do `livekit-server` para desenvolvimento local.

## Primeiros passos

```powershell
Copy-Item .env.example .env
cargo ci
livekit-server --dev
cargo server
cargo client            # servidor de tokens real, sem mídia
cargo client -- --demo  # tudo simulado, sem servidor
```

## Comandos

| Comando | O que faz |
| --- | --- |
| `cargo ci` | fmt, clippy com avisos como erro, e testes |
| `cargo lint` | só o clippy |
| `cargo server` | sobe o `rooms-token-server` (`cargo run -p rooms-token-server -- hash-password` gera `ROOMS_INVITE_CODE_HASH`) |
| `cargo client` | abre o `rooms-client` |
| `cargo fmt --all` | formata o código |

## Testes ponta a ponta

No Windows, `scripts\e2e.cmd` (duplo clique ou `scripts\e2e.cmd --filter playout`) sobe o `livekit-server` e o servidor de tokens, roda bots por script e mede, com loopback do WASAPI, se o áudio remoto chega ao dispositivo de saída. O relatório fica em `dev-logs\e2e\report.md` (e `report.json`) e a saída do console em `dev-logs\e2e\console.log`. **Um tom de teste audível, em volume baixo, toca durante a execução.** Detalhes e limites em [docs/status.md](docs/status.md).

## Estrutura

| Caminho | Conteúdo |
| --- | --- |
| `crates/rooms-protocol` | Tipos compartilhados entre cliente e servidor |
| `crates/rooms-core` | Sessão, áudio, captura e vídeo; única crate que conhece o SDK do LiveKit |
| `crates/rooms-client` | Interface desktop em egui |
| `crates/rooms-token-server` | Servidor HTTP que emite tokens |
| `crates/rooms-e2e` | Harness de testes ponta a ponta: bots, sonda de áudio e relatório |
| `xtask` | Automação do repositório (`cargo ci`) |
| `deploy` | Configuração do servidor em produção |
| `docs` | Plano, arquitetura e decisões (ADRs) |

## Convenções

Veja [CONTRIBUTING.md](CONTRIBUTING.md) para padrões de código, commits e fluxo de trabalho.
