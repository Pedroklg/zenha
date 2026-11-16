# Padrões de código e arquitetura do Zenha

Regras aplicadas no desenvolvimento e no code review. Cada regra tem um ID estável usado pelos checks automáticos (`cargo review` / `xtask`) e pela skill de review.

| Arquivo | Escopo | IDs |
|---|---|---|
| [general.md](general.md) | Todo código Rust do workspace (`crates/*`, `xtask`) | `GER-01` a `GER-12` |
| [core-media.md](core-media.md) | `crates/rooms-core` (LiveKit, `unsafe`, áudio, vídeo, Tokio) | `CORE-01` a `CORE-12` |
| [client-ui.md](client-ui.md) | `crates/rooms-client` (`eframe`/`egui`, thread da UI, estado) | `UI-01` a `UI-09` |
| [protocol.md](protocol.md) | `crates/rooms-protocol` (`serde`, limites, compatibilidade) | `PROTO-01` a `PROTO-05` |
| [token-server.md](token-server.md) | `crates/rooms-token-server` (`axum`, Argon2, JWT, SQLite) | `SRV-01` a `SRV-08` |
| [testing.md](testing.md) | Testes unitários, integração e `crates/rooms-e2e` | `TST-01` a `TST-07` |
| [ci-deploy.md](ci-deploy.md) | `.github/workflows/`, `deploy/`, `installer/`, `xtask` | `OPS-01` a `OPS-06` |
| [code-review.md](code-review.md) | Guia de review: checklist bloqueante e divisão máquina × revisor | — |

## Escopo

- As regras valem para **código novo e código tocado na branch ou task**.
- Código legado fora do diff não é migrado de carona; ocorrência irmã só entra no review quando faz parte da mesma mudança ou quando a alteração quebrou um contrato compartilhado.
- Ao tocar um módulo grande já existente (como `crates/rooms-client/src/state/` ou `crates/rooms-core/src/session/`), não agrave o acoplamento e extraia lógica pura nova em funções testáveis, sem exigir reescrita do módulo inteiro.

## Severidades

| Severidade | Quando usar |
|---|---|
| **bloqueante** | Bug funcional, pânico potencial em produção, bloqueio da thread de UI (`egui`) ou worker Tokio, UB em `unsafe`, vazamento de segredo/token, quebra de fronteira entre crates, validação de segurança ausente no servidor ou teste que confirma comportamento errado |
| **improvement** | Viola uma regra documentada nesta pasta, introduz clone/alocação evitável em caminho quente de mídia ou deixa lógica pura nova sem teste |
| **observation** | Nome, comentário, organização local ou sugestão de legibilidade |
| **dúvida** | Falta contexto do requisito, do hardware no Windows ou da API do SDK para afirmar o comportamento |

## Formato de achado no review

Ordenado da maior severidade para a menor (`bloqueante` -> `improvement` -> `observation` -> `dúvida`):

```text
<arquivo>:<linha> — <ID da regra> — <severidade>
Problema: <o problema em uma frase direta>
Correção: <mudança concreta com snippet ou pontos afetados>
```
