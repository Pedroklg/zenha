# Regras de CI, empacotamento e deploy (OPS)

Complementa [general.md](general.md). Escopo: `.github/workflows/`, `deploy/`, `installer/`, `scripts/`, `xtask/`, `Cargo.toml` e `deny.toml`.

## OPS-01: Paridade entre `cargo ci` local, `cargo review` e o workflow de CI

- `cargo ci` (`xtask/src/main.rs`) centraliza a verificação, que roda só no PC (não há workflow de build ou teste no GitHub Actions): checagens de fronteira, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` e `cargo deny check`.
- Dependências em `Cargo.toml` devem respeitar `deny.toml` (licenças permitidas, `wildcards = "deny"`, fontes restritas ao `crates.io` e ao repositório git fixado do `DeepFilterNet`, sem advisories não justificados e respeitando `[bans] wrappers`).

## OPS-02: Dockerfile reproduzível (`--locked`) e sem privilégio de root

- `deploy/Dockerfile` compila `rooms-token-server` com `cargo build --release --locked --package rooms-token-server` na mesma versão do compilador definida em `rust-toolchain.toml`.
- A imagem final (`debian:stable-slim`) instala apenas `ca-certificates` com `--no-install-recommends`, cria usuário sem privilégio `zenha:zenha` (`10001:10001`, `/usr/sbin/nologin`), ajusta `/var/lib/zenha` e executa sob `USER zenha:zenha`.

## OPS-03: Imagens fixadas e segredos restritos à VM no deploy

- Em `deploy/docker-compose.yml` e `deploy/setup-server.sh`, imagens de infraestrutura (`livekit/livekit-server`, `caddy`) usam tags de versão explícitas, nunca `:latest`.
- Segredos de produção residem apenas na VM em `/opt/zenha/.env` e `/opt/zenha/livekit.yaml` com permissão restrita (`0600`).
- Workflows em `.github/workflows/` nunca imprimem segredos em stdout, validam tags antes de interpolar em scripts remotos e validam `/health` após o deploy (`deploy.yml`).

## OPS-04: Segurança de rede e infraestrutura Azure (`deploy/azure/`)

- No Bicep (`deploy/azure/main.bicep`), apenas as portas estritamente necessárias (`80/TCP`, `443/TCP`, `7881/TCP`, `7882/UDP`) ficam abertas para a internet; SSH (`22/TCP`) permanece fechado por padrão (`sshAllowedCidr = ''`).
- `rooms-token-server` e a porta HTTP interna do `livekit-server` escutam apenas em loopback na VM, expostos externamente exclusivamente via TLS no Caddy.

## OPS-05: Release validada antes de empacotar

- `scripts/release.cmd X.Y.Z` publica a release do PC: exige estar na `main` limpa, versão igual a `[workspace.package].version` e tag `vX.Y.Z` inexistente, roda `cargo ci`, gera instalador e zip (`scripts/package.cmd`, `installer/zenha.iss`), faz push e cria a release com `gh`; a tag dispara `server-image.yml` e depois `deploy.yml`.

## OPS-06: Arquivos proibidos e `.gitignore`

- `.env`, `.env.*` (exceto `.env.example`), `*.tfvars`, `*.tfvars.json`, `*.pem`, `*.key` e `deploy/livekit.prod.yaml` permanecem no `.gitignore`, nunca são lidos nem versionados.
