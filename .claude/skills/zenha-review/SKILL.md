---
name: zenha-review
description: Code review de commit, branch ou worktree de agente do Zenha contra docs/standards/. Roda cargo review e cargo clippy, lê o código alterado e devolve só os achados, ordenados por severidade, no formato que o autor do código corrige direto. Use quando pedirem review, CR ou revisão de task/worktree.
argument-hint: "[commit | base [head]]"
---

# /zenha-review

Revisa uma mudança do Zenha (Rust, Windows) contra `docs/standards/` e devolve **só os achados**.

Somente leitura: não edite arquivos, não rode `cargo fmt` sem `--check` nem `cargo clippy --fix`, não faça commit, push nem checkout. Nunca leia `.env`, `.env.*` (exceto `.env.example`), `*.tfvars`, `*.pem`, `*.key` nem `deploy/livekit.prod.yaml`; credencial no diff vira achado com `arquivo:linha`, sem transcrever o valor.

## 1. Diff

Escolha a base (`<base>`) e o diff:
- **Worktree de agente** (`git status --porcelain` mostra mudanças): `<base> = HEAD`; diff = `git diff HEAD` mais os arquivos novos não ignorados. Ignore `AGY_*.md` e `AGY_*.txt` como código; se houver `AGY_BRIEF.md`, leia-o para conferir se o pedido foi feito e se o escopo foi respeitado.
- **Commit** (`<sha>`): `<base> = <sha>~1`; diff = `git show <sha>`.
- **Branch**: `<base>` = argumento ou `git merge-base main HEAD`; diff = `git diff <base>...<head>` (`head` padrão `HEAD`).

## 2. Ferramentas

Rode em sequência, nunca dois comandos cargo ao mesmo tempo:
1. `cargo review <base>` (alias de `cargo xtask review-checks`; a base só serve à checagem `CORE-12`).
2. `cargo clippy --workspace --all-targets -- -D warnings`.

Cada `error`/`warning` dessas saídas em arquivo ou linha do diff entra na lista como achado (`error` = bloqueante, `warning` = improvement), no mesmo formato da seção 4. Não repita o mesmo problema como achado manual. Se um comando não rodar, diga só isso na primeira linha da resposta.

## 3. Leitura

1. Leia `docs/standards/README.md`, `docs/standards/code-review.md` e `docs/standards/general.md`.
2. Leia o arquivo de regras de cada área tocada:
   - `crates/zenha-core/` → `docs/standards/core-media.md`
   - `crates/zenha-client/` → `docs/standards/client-ui.md`
   - `crates/zenha-protocol/` → `docs/standards/protocol.md`
   - `crates/zenha-server/` → `docs/standards/token-server.md`
   - `crates/zenha-e2e/` e testes → `docs/standards/testing.md`
   - `deploy/`, `.github/`, `installer/`, `xtask/`, `Cargo.toml`, `deny.toml` → `docs/standards/ci-deploy.md`
3. Leia a função inteira em volta de cada hunk e, quando preciso, quem chama, os testes do módulo e os pontos de ligação de `Command`/`Event` (`CORE-12`).
4. Passe pelo checklist de bloqueantes de `docs/standards/code-review.md`.
5. Só reporte o que você confirmou no código atual: a linha existe, foi introduzida ou agravada pelo diff, e o caso não é tratado pelo chamador, pela função chamada nem pelo compilador (`match` exaustivo, tipos).

## 4. Saída

Só a lista de achados, da maior severidade para a menor (`bloqueante`, `improvement`, `observation`, `dúvida`). Sem resumo, sem elogio, sem o que passou, sem seção de "talvez". Um bloco por achado:

```text
<arquivo>:<linha> — <ID da regra> — <severidade>
Problema: <uma frase>
Correção: <mudança concreta; snippet curto ou os arquivo:linha que mudam junto>
```

Sem achados, responda só: `Nenhum achado.`
