# Deploy

See [docs/deploy.md](../docs/deploy.md) for the complete OpenTofu infrastructure-as-code, Cloudflare DNS, server bootstrap, GitHub Actions OIDC deployment, budget alerts, latency measurement, and cost guide.

## Files in `deploy/`

- [`infra/modules/host/`](infra/modules/host/) — Provider-neutral host module defining the shared inbound port list (`ports`) and [`cloud-init.yaml`](infra/modules/host/cloud-init.yaml) (installs Docker Engine + Compose plugin from official repositories, creates `/opt/zenha`, and enables unattended security upgrades).
- [`infra/azure/`](infra/azure/) — OpenTofu stack for Azure (VNet, subnet, NSG, static Standard Public IP with DNS label, NIC, `Standard_B2ats_v2` Ubuntu 24.04 LTS VM, and optional start/deallocate Logic App schedules). See [`infra/azure/README.md`](infra/azure/README.md) for adopting an existing Bicep deployment via `tofu import`.
- [`infra/hetzner/`](infra/hetzner/) — OpenTofu stack for Hetzner Cloud (server, firewall, static Primary IPv4, SSH key, and cloud-init; no automatic scheduling).
- [`Dockerfile`](Dockerfile) — Multi-stage build for `rooms-token-server` (runs as non-root `zenha` user).
- [`docker-compose.yml`](docker-compose.yml) — Production stack in `/opt/zenha` (`livekit/livekit-server:v1.13.11`, `rooms-token-server` from GHCR via `ZENHA_IMAGE`, `caddy:2.11.9-alpine`).
- [`Caddyfile`](Caddyfile) — Reverse proxy for `{$TOKEN_DOMAIN}` (`127.0.0.1:8787`) and `{$RTC_DOMAIN}` (`127.0.0.1:7880`) with automatic HTTPS.
- [`livekit.example.yaml`](livekit.example.yaml) — Reference LiveKit configuration (`7880` signaling, `7881/TCP`, `7882/UDP`, `use_external_ip` / `node_ip`).
- [`setup-server.sh`](setup-server.sh) — Idempotent one-time VM bootstrap script (generates LiveKit keys, writes `/opt/zenha/.env` and `/opt/zenha/livekit.yaml` with mode `0600`, and starts Docker Compose).
