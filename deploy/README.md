# deploy

Server configuration and infrastructure. The step-by-step guide is [docs/deploy.md](../docs/deploy.md).

| Path | Contents |
| --- | --- |
| [`infra/azure/`](infra/azure/README.md) | OpenTofu stack for Azure: network, NSG, static IP, `Standard_B2ats_v2` VM, optional start/stop schedule. Also explains adopting an old Bicep deployment. |
| [`infra/hetzner/`](infra/hetzner/) | OpenTofu stack for Hetzner Cloud: server, firewall, static IPv4, SSH key. No schedule. |
| [`infra/modules/host/`](infra/modules/host/) | Shared inbound port list and [`cloud-init.yaml`](infra/modules/host/cloud-init.yaml) (Docker Engine, Compose plugin, `/opt/zenha`, unattended upgrades). |
| [`Dockerfile`](Dockerfile) | Multi-stage build of `rooms-token-server`, runs as the non-root `zenha` user. |
| [`docker-compose.yml`](docker-compose.yml) | Production stack in `/opt/zenha`: `livekit-server`, the token server image from `ZENHA_IMAGE`, Caddy. |
| [`Caddyfile`](Caddyfile) | HTTPS for `{$TOKEN_DOMAIN}` (to `127.0.0.1:8787`) and `{$RTC_DOMAIN}` (to `127.0.0.1:7880`). |
| [`livekit.example.yaml`](livekit.example.yaml) | Reference LiveKit config: `7880` signaling, `7881/TCP`, `7882/UDP`, `use_external_ip` or `node_ip`. |
| [`setup-server.sh`](setup-server.sh) | One-time, rerunnable VM bootstrap: LiveKit keys, `/opt/zenha/.env` and `livekit.yaml` (mode `0600`), `docker compose up`. |
