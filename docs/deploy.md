# Deploying Zenha to Azure

This guide covers provisioning and operating the Zenha voice/video server (`livekit-server`, `rooms-token-server`, and Caddy 2) on a single Azure Linux virtual machine using Bicep, Cloudflare DNS, and GitHub Actions.

---

## 1. Architecture Overview

- **Cloud & Region:** Azure (`chilecentral` by default, the region enabled by Azure for Students policies for free burstable B-series v2 VMs).
- **Compute & Storage:** `Standard_B2ats_v2` (2 vCPUs, 1 GiB RAM), Ubuntu Server 24.04 LTS x64, 30 GiB Standard SSD (`StandardSSD_LRS`) OS disk, static Standard SKU Public IPv4 address.
- **Containers (`/opt/zenha/docker-compose.yml`, host networking):**
  - `livekit/livekit-server:v1.13.11` — WebRTC SFU listening on `127.0.0.1:7880` (HTTP/WS signaling behind Caddy), `7881/TCP` (ICE/TCP fallback), and `7882/UDP` (single-port UDP media mux).
  - `ghcr.io/<owner>/zenha-token-server:<tag>` — Rust `axum` token server listening on `127.0.0.1:8787`.
  - `caddy:2.11.9-alpine` — Automatic HTTPS reverse proxy listening on `80/TCP` and `443/TCP`, routing `TOKEN_DOMAIN` to `127.0.0.1:8787` and `RTC_DOMAIN` to `127.0.0.1:7880`.
- **Secrets:** LiveKit API key/secret and the Argon2 invite password hash (`ROOMS_INVITE_CODE_HASH`) exist **only on the VM** in `/opt/zenha/.env` and `/opt/zenha/livekit.yaml` (`chmod 0600`). CI/CD only updates the `ZENHA_IMAGE` tag and restarts the Compose stack.
- **Firewall (NSG inbound rules):**
  - `80/TCP` — ACME HTTP-01 challenge and HTTP-to-HTTPS redirect.
  - `443/TCP` — HTTPS (`rooms-token-server`) and WSS (`livekit-server` signaling).
  - `7881/TCP` — WebRTC ICE over TCP fallback.
  - `7882/UDP` — WebRTC multiplexed UDP media.
  - `22/TCP` (SSH) — **Closed by default**; optionally restricted to a single operator IP/CIDR via the `sshAllowedCidr` Bicep parameter.

---

## 2. Prerequisites

1. **Azure Subscription** (for example, Azure for Students) with permission to deploy in `chilecentral`.
2. **Azure CLI (`az`)** v2.60+ with the Bicep CLI installed:
   ```bash
   az version
   az bicep install
   az bicep version
   ```
3. **Cloudflare DNS Zone** for your domain (referred to below as `voice.example.com` for the token server and `rtc.voice.example.com` for LiveKit).
4. **SSH Key Pair** for the VM administrator account:
   ```bash
   ssh-keygen -t ed25519 -C "zenha-azure" -f ~/.ssh/zenha_azure
   ```
5. **Published Token Server Image on GHCR:**
   - Run the `server-image` workflow (`.github/workflows/server-image.yml`) through the `release` workflow, by pushing a tag (`v0.1.0`) or by triggering it manually via **Actions -> server-image -> Run workflow**.
   - In GitHub, open the package **`zenha-token-server`** -> **Package settings** -> **Change visibility** -> **Public** so the VM can pull `ghcr.io/<owner>/zenha-token-server:<tag>` without storing a registry token on the server.

---

## 3. Provisioning the VM with Bicep (`az deployment group create`)

1. Log in to Azure and select your subscription:
   ```bash
   az login
   az account set --subscription "<SUBSCRIPTION_ID>"
   ```

2. Create a resource group in `chilecentral`:
   ```bash
   az group create \
     --name rg-zenha-prod \
     --location chilecentral
   ```

3. Edit `deploy/azure/main.bicepparam` (or pass parameters on the command line) with your SSH public key and a globally unique DNS label for the public IP:
   - `sshPublicKey`: contents of `~/.ssh/zenha_azure.pub`
   - `dnsLabelPrefix`: unique label (produces `<dnsLabelPrefix>.chilecentral.cloudapp.azure.com`)
   - `sshAllowedCidr`: leave `''` to keep SSH port 22 closed, or set `'<YOUR_PUBLIC_IP>/32'` if you need direct SSH access

4. Validate and deploy the Bicep template:
   ```bash
   az deployment group create \
     --resource-group rg-zenha-prod \
     --template-file deploy/azure/main.bicep \
     --parameters deploy/azure/main.bicepparam
   ```

5. Read the deployment outputs (`publicIpAddress`, `fqdn`, `vmName`):
   ```bash
   az deployment group show \
     --resource-group rg-zenha-prod \
     --name main \
     --query properties.outputs \
     --output table
   ```

6. `deploy/azure/cloud-init.yaml` runs automatically on first boot to install Docker Engine, the Docker Compose plugin, and `unattended-upgrades`, and to create `/opt/zenha`. You can verify `cloud-init` finished using `run-command`:
   ```bash
   az vm run-command invoke \
     --resource-group rg-zenha-prod \
     --name zenha-vm \
     --command-id RunShellScript \
     --scripts "cloud-init status --wait"
   ```

---

## 4. Cloudflare DNS Records

In the Cloudflare dashboard for your zone, create two **A** records pointing to the `publicIpAddress` output from Bicep:

| Type | Name | Content (IPv4) | Proxy status | TTL |
| :--- | :--- | :--- | :--- | :--- |
| `A` | `<token-subdomain>` (e.g. `voice`) | `<publicIpAddress>` | **DNS only (grey cloud)** | Auto |
| `A` | `<rtc-subdomain>` (e.g. `rtc.voice`) | `<publicIpAddress>` | **DNS only (grey cloud)** | Auto |

> **Important:** Both records **must** have Cloudflare Proxy disabled (**DNS only / grey cloud**). Cloudflare's HTTP proxy does not forward LiveKit's raw WebRTC media ports (`7881/TCP` and `7882/UDP`), and Caddy obtains Let's Encrypt TLS certificates directly on ports `80/TCP` and `443/TCP`.

Verify DNS propagation before running `setup-server.sh` so Caddy can immediately issue TLS certificates:
```bash
nslookup voice.example.com 1.1.1.1
nslookup rtc.voice.example.com 1.1.1.1
```

---

## 5. Initial Server Bootstrap (`deploy/setup-server.sh`)

`deploy/setup-server.sh` is an idempotent bootstrap script that runs once on the VM. It:
1. Installs `/opt/zenha/docker-compose.yml` and `/opt/zenha/Caddyfile`.
2. Generates a fresh LiveKit production API key and secret via `livekit/livekit-server:v1.13.11 generate-keys` (or preserves the existing key pair if `/opt/zenha/.env` already exists).
3. Accepts a precomputed Argon2id PHC hash (`ROOMS_INVITE_CODE_HASH`) or hashes a plaintext password on the VM using `rooms-token-server hash-password`.
4. Writes `/opt/zenha/livekit.yaml` and `/opt/zenha/.env` with `0600` (`root:root`) permissions.
5. Runs `docker compose pull && docker compose up -d` and verifies `http://127.0.0.1:8787/health`.

### Step 5.1 — Generate the Argon2 Invite Code Hash Locally

Generate the PHC string (`$argon2id$v=19$...`) on your workstation so the plaintext password never appears in Azure CLI logs:
```bash
echo "your-invite-password" | cargo run --quiet -p rooms-token-server -- hash-password
```

### Step 5.2 — Run `setup-server.sh` via `az vm run-command invoke` (No SSH Required)

`run-command` does not forward `KEY=value` arguments to the script, so put the settings at the top
of a copy of the script and send that file. Keep the shebang on the first line: without it the
agent runs the script with `sh`, which rejects `set -o pipefail`.

```bash
{
  echo '#!/usr/bin/env bash'
  echo "export TOKEN_DOMAIN='voice.example.com'"
  echo "export RTC_DOMAIN='rtc.voice.example.com'"
  echo "export ZENHA_IMAGE='ghcr.io/<owner>/zenha-token-server:v0.1.0'"
  echo "export NODE_IP='<publicIpAddress>'"
  echo 'export ROOMS_INVITE_CODE_HASH='\''$argon2id$v=19$m=19456,t=2,p=1$...'\'
  tail -n +2 deploy/setup-server.sh
} > /tmp/zenha-setup.sh

az vm run-command invoke \
  --resource-group rg-zenha-prod \
  --name zenha-vm \
  --command-id RunShellScript \
  --scripts @/tmp/zenha-setup.sh \
  --query 'value[0].message' -o tsv
rm /tmp/zenha-setup.sh
```

> **Note on `rtc.use_external_ip` vs `rtc.node_ip`:** By default, `setup-server.sh` sets `use_external_ip: true` in `/opt/zenha/livekit.yaml` so LiveKit discovers the VM's public IPv4 via STUN across Azure's 1:1 NAT. If you prefer to pin the static public IPv4 directly without STUN, pass `"NODE_IP=<publicIpAddress>"` as an additional parameter; `setup-server.sh` will set `use_external_ip: false` and `node_ip: "<publicIpAddress>"`.

### Step 5.3 — Alternative: Run `setup-server.sh` over SSH

If you enabled `sshAllowedCidr` for your workstation IP:
```bash
scp -i ~/.ssh/zenha_azure deploy/setup-server.sh deploy/docker-compose.yml deploy/Caddyfile azureuser@<publicIpAddress>:/tmp/
ssh -i ~/.ssh/zenha_azure azureuser@<publicIpAddress> \
  "sudo TOKEN_DOMAIN=voice.example.com RTC_DOMAIN=rtc.voice.example.com ZENHA_IMAGE=ghcr.io/<owner>/zenha-token-server:v0.1.0 bash /tmp/setup-server.sh"
```
When run interactively in a terminal without `ROOMS_INVITE_CODE_HASH`, the script prompts silently for either the Argon2 PHC hash or the plaintext invite password.

---

## 6. Configuring GitHub Actions OIDC Deployment

`.github/workflows/deploy.yml` authenticates to Azure using OpenID Connect (`azure/login@v3`) with no long-lived client secrets, scoped to the GitHub `production` environment.

### Step 6.1 — Create the Microsoft Entra App Registration and Service Principal

```bash
SUBSCRIPTION_ID="$(az account show --query id -o tsv)"
TENANT_ID="$(az account show --query tenantId -o tsv)"
RG_NAME="rg-zenha-prod"
VM_NAME="zenha-vm"
GH_OWNER="<github-owner>"
GH_REPO="<github-repo>"

APP_ID="$(az ad app create --display-name "github-zenha-deploy" --query appId -o tsv)"
az ad sp create --id "${APP_ID}"

az role assignment create \
  --assignee "${APP_ID}" \
  --role "Virtual Machine Contributor" \
  --scope "/subscriptions/${SUBSCRIPTION_ID}/resourceGroups/${RG_NAME}/providers/Microsoft.Compute/virtualMachines/${VM_NAME}"
```

### Step 6.2 — Add the Federated Credential for GitHub Actions

Because `.github/workflows/deploy.yml` declares `environment: production`, the OIDC subject claim is `repo:<owner>/<repo>:environment:production`. Newer repositories may send it with immutable IDs instead (`repo:<owner>@<owner_id>/<repo>@<repo_id>:environment:production`). If the first deploy fails with `AADSTS700213`, the error prints the exact subject GitHub sent; register that one:

```bash
az ad app federated-credential create \
  --id "${APP_ID}" \
  --parameters "{
    \"name\": \"github-production-env\",
    \"issuer\": \"https://token.actions.githubusercontent.com\",
    \"subject\": \"repo:${GH_OWNER}/${GH_REPO}:environment:production\",
    \"description\": \"GitHub Actions production deployment\",
    \"audiences\": [\"api://AzureADTokenExchange\"]
  }"
```

### Step 6.3 — Configure the GitHub `production` Environment

In your GitHub repository, navigate to **Settings -> Environments -> New environment**, create **`production`**, and add the following secrets or variables:

| Name | Where | Value |
| :--- | :--- | :--- |
| `AZURE_CLIENT_ID` | Secret or Variable | `${APP_ID}` |
| `AZURE_TENANT_ID` | Secret or Variable | `${TENANT_ID}` |
| `AZURE_SUBSCRIPTION_ID` | Secret or Variable | `${SUBSCRIPTION_ID}` |
| `AZURE_RESOURCE_GROUP` | Variable (or Secret) | `rg-zenha-prod` |
| `AZURE_VM_NAME` | Variable (or Secret) | `zenha-vm` |

---

## 7. SSH & Local CLI Alternatives (If Azure for Students Blocks App Registrations)

Many university-managed **Azure for Students** directory tenants restrict students from creating Microsoft Entra ID App Registrations (`Insufficient privileges to complete the operation` when running `az ad app create`). If your tenant blocks OIDC setup, use either of these two alternatives:

### Alternative A — Trigger Updates from Your Local Azure CLI (SSH Remains Closed)

You do **not** need an App Registration or open SSH port `22/TCP` to update the VM from your own machine. After `.github/workflows/server-image.yml` publishes a new image tag to GHCR, run:

```bash
az vm run-command invoke \
  --resource-group rg-zenha-prod \
  --name zenha-vm \
  --command-id RunShellScript \
  --scripts '
    set -euo pipefail
    sed -i "s|^ZENHA_IMAGE=.*|ZENHA_IMAGE=$1|" /opt/zenha/.env
    chmod 0600 /opt/zenha/.env
    cd /opt/zenha && docker compose pull && docker compose up -d --remove-orphans
  ' \
  --parameters "ghcr.io/<owner>/zenha-token-server:v0.1.0"
```

### Alternative B — Deploy over SSH

1. Set `sshAllowedCidr = '<YOUR_PUBLIC_IP>/32'` in `deploy/azure/main.bicepparam` and re-run `az deployment group create`.
2. Update the server directly over SSH:
   ```bash
   ssh -i ~/.ssh/zenha_azure azureuser@<publicIpAddress> '
     sudo sed -i "s|^ZENHA_IMAGE=.*|ZENHA_IMAGE=ghcr.io/<owner>/zenha-token-server:v0.1.0|" /opt/zenha/.env &&
     cd /opt/zenha &&
     sudo docker compose pull &&
     sudo docker compose up -d --remove-orphans
   '
   ```

---

## 8. Budget Alerts (`az consumption budget` and Azure Portal)

Azure for Students includes a **$100 USD credit valid for 12 months**. Configure a budget alert immediately so unexpected egress or non-free SKU charges never exhaust your credit silently.

### Option A — Azure Portal (Recommended for Azure for Students)

1. Open the [Azure Portal](https://portal.azure.com/) and go to **Cost Management + Billing** -> **Cost Management** -> **Budgets** (or **Education** -> **Overview** to see remaining student credit).
2. Click **+ Add**:
   - **Scope:** Your Azure for Students subscription.
   - **Name:** `zenha-monthly-budget`
   - **Reset period:** Monthly
   - **Amount:** `10` (USD)
3. Configure **Alert conditions** at **50%** ($5), **80%** ($8), and **100%** ($10) of budget (both **Actual** and **Forecasted**), and enter your email address under **Alert recipients**.

### Option B — Azure CLI (`az consumption budget create`)

> **Note:** On some Azure for Students sponsorship offers (`MS-AZR-0170P`), the `Microsoft.Consumption/budgets` REST API returns an unsupported offer error from the CLI; use the Azure Portal / Education Hub if that happens.

```bash
az consumption budget create \
  --budget-name "zenha-monthly-budget" \
  --category "cost" \
  --amount 10.0 \
  --time-grain "monthly" \
  --start-date "2026-11-01" \
  --end-date "2027-11-01"
```

---

## 9. Updating the Server

### Automatic Release Deployment

Bump `[workspace.package].version` in `Cargo.toml`, merge to `main`, then run `gh workflow run release.yml --ref main -f version=0.2.0` (or **Actions -> release -> Run workflow**). The `release` workflow checks that the version matches `Cargo.toml`, that tag `v0.2.0` does not exist yet and that `ci` passed on that commit, builds the installer and zip, creates the `v0.2.0` tag and GitHub Release, and then starts:
1. `.github/workflows/server-image.yml` — builds `deploy/Dockerfile` and pushes `ghcr.io/<owner>/zenha-token-server:v0.2.0` and `:latest`.
2. `.github/workflows/deploy.yml` — runs automatically after `server-image` succeeds (`workflow_run`), logs in to Azure via OIDC, updates `ZENHA_IMAGE` in `/opt/zenha/.env`, runs `docker compose pull && docker compose up -d --remove-orphans`, and verifies `/health`.

### Manual Deployment (`workflow_dispatch`)

In GitHub **Actions -> deploy -> Run workflow**, enter the desired image tag (for example `v0.2.0` or `latest`) to redeploy or roll back at any time.

### Updating LiveKit or Caddy Versions

Edit the pinned image tags in `deploy/docker-compose.yml` (and `deploy/setup-server.sh`), then re-run `deploy/setup-server.sh` (it preserves existing keys and hashes in `/opt/zenha/.env`).

---

## 10. Measuring Latency and Verifying Health

### 10.1 Token Server Health and HTTPS Latency

From any client machine, measure DNS lookup, TCP connect, TLS handshake, and HTTP response time to `chilecentral`:
```bash
curl -sS -o /dev/null -w "dns=%{time_namelookup}s tcp=%{time_connect}s tls=%{time_appconnect}s ttft=%{time_starttransfer}s total=%{time_total}s\n" \
  https://voice.example.com/health
```
Subtracting `time_namelookup` from `time_connect` (`tcp - dns`) gives the one-way TCP SYN/SYN-ACK round-trip time (RTT) between the client and the `chilecentral` VM.

### 10.2 Testing LiveKit Signaling and ICE/TCP Fallback on Windows

In PowerShell on Windows:
```powershell
# Measure TCP handshake latency to HTTPS/WSS (443) and LiveKit ICE/TCP (7881)
Test-NetConnection -ComputerName rtc.voice.example.com -Port 443
Test-NetConnection -ComputerName rtc.voice.example.com -Port 7881
```
*(Note: Azure blocks inbound ICMP echo requests outside the VNet by default unless explicitly permitted, so TCP connect timing via `curl` or `Test-NetConnection` is more reliable than `ping`.)*

### 10.3 Inspecting LiveKit WebRTC Logs on the Server

```bash
az vm run-command invoke \
  --resource-group rg-zenha-prod \
  --name zenha-vm \
  --command-id RunShellScript \
  --scripts "cd /opt/zenha && docker compose logs --tail=100 livekit token-server caddy"
```

---

## 11. Expected Costs and Pricing References

### 11.1 Monthly Cost Breakdown in `chilecentral` (USD, 730 hours/month)

| Resource | SKU / Configuration | Azure for Students Free Tier Inclusion | Estimated Pay-As-You-Go Rate (`chilecentral`) | Expected Monthly Charge Against $100 Credit |
| :--- | :--- | :--- | :--- | :--- |
| **Virtual Machine** | `Standard_B2ats_v2` (Linux, 2 vCPU, 1 GiB RAM) | **750 hours/month free** for 12 months (eligible burstable B-series v2 VM) | ~$0.0132 / hr (~$9.64 / mo) | **$0.00 / mo** (while running 1 instance <= 750 hrs/mo) |
| **OS Managed Disk** | `StandardSSD_LRS` 30 GiB (billed as **E4** 32 GiB tier) | Free services include two 64 GiB (P6) managed disks; if E4 is billed separately, ~$2.40–$3.00 / mo | ~$2.40 – $3.00 / mo | **$0.00 – $3.00 / mo** (see unverified note below) |
| **Public IPv4 Address** | `Standard` SKU, `Static`, Regional | Free tier specifies Basic/Dynamic IP (retired for new deployments); Standard Static IP is billed | **$0.005 / hr** (~**$3.65 / mo** at 730 hrs) | **~$3.65 / mo** |
| **Virtual Network & NSG** | 1 VNet, 1 Subnet, 1 NSG | Free | Free | **$0.00 / mo** |
| **Inbound Data Transfer** | All inbound traffic | Free | $0.00 / GB | **$0.00 / mo** |
| **Outbound Data Transfer (Egress)** | Internet egress from `chilecentral` | **15 GB / month free** (Azure Free / Student account allowance; 5 GB/mo on standard PAYG) | **~$0.12 – $0.16 / GB** after free allowance in South America | **$0.00** up to 15 GB/mo; **~$0.12–$0.16 per additional GB** |

**Estimated Baseline Fixed Cost against the $100 Student Credit:** **~$3.65 to $6.65 USD / month** (plus any outbound bandwidth above 15 GB/month). Even if both the Standard Static Public IP and the E4 Standard SSD disk are deducted from the $100 credit, the $100 credit covers a full 12-month year with ~$20–$55 left over for outbound WebRTC media bandwidth.

### 11.2 WebRTC Bandwidth Estimation

- **Voice call (Opus ~32–48 kbps per speaker):** If 1 person speaks to 5 listeners (5 outbound streams from the SFU), the server sends ~200 kbps (~90 MB/hour). 15 GB of free monthly egress covers **~165 hours** of 6-person voice calls per month.
- **Screen share (1080p30 VP9 at ~2.5 Mbps to 3 viewers):** The SFU sends ~7.5 Mbps (~3.37 GB/hour). Heavy screen-sharing sessions will exceed 15 GB/month and draw ~$0.40–$0.54/hour of 3-viewer 1080p30 streaming from the $100 student credit.

### 11.3 What Could Not Be Confirmed Without Live Account Billing Access

1. **Exact `chilecentral` meter rates on `MS-AZR-0170P` (Azure for Students):** Microsoft's public pricing pages require interactive JavaScript/API queries per currency and offer ID, and South America (`chilecentral`) regional multipliers for `StandardSSD_LRS` E4 and post-15 GB egress can vary by a few cents from `brazilsouth` / US baselines. Check **Cost Management -> Cost analysis** in the Azure Portal after the first 24–48 hours of operation.
2. **Managed Disk Free-Tier SKU Matching (`StandardSSD_LRS` E4 vs `Premium_LRS` P6):** Azure Free Services documentation lists two 64 GiB (P6) disks under free monthly services, while some regional student policies zero-rate `StandardSSD_LRS` up to 64 GiB. Verify in **Education Hub -> Free services** whether your 30 GiB `StandardSSD_LRS` disk is zero-rated or billed at ~$2.40–$3.00/month.

### 11.4 Pricing & Documentation Sources

- Azure for Students program & free services: <https://learn.microsoft.com/en-us/azure/education-hub/azure-for-students-program> and <https://azure.microsoft.com/en-us/free/students/>
- Azure Free Account FAQ (750 hours B2ats v2, 15 GB outbound data transfer, managed disks): <https://azure.microsoft.com/en-us/free/free-account-faq/>
- Azure Virtual Machines pricing: <https://azure.microsoft.com/en-us/pricing/details/virtual-machines/linux/>
- Azure Managed Disks pricing: <https://azure.microsoft.com/en-us/pricing/details/managed-disks/>
- Azure IP Addresses pricing ($0.005/hr per Standard Public IPv4): <https://azure.microsoft.com/en-us/pricing/details/ip-addresses/>
- Azure Bandwidth (Data Transfer) pricing: <https://azure.microsoft.com/en-us/pricing/details/bandwidth/>

---

## 12. Saved Chat Messages

When a user marks a chat message to be saved, `rooms-token-server` persists it in SQLite (`WAL` mode) at the path configured by `ROOMS_CHAT_DB`.

- **Storage location:** In production (`deploy/docker-compose.yml`), `ROOMS_CHAT_DB` is set to `/var/lib/zenha/chat.db`, backed by the Docker named volume `zenha_data` (created under `/opt/zenha` as `zenha_zenha_data` by Docker Compose).
- **Plaintext and retention:** Saved messages are stored in **plaintext** in `chat.db` with **no automatic expiration**. Unsaved messages never reach the token server. Senders can delete their own saved messages via the API (`DELETE /rooms/{room}/chat/{id}`).
- **Backup:** Use SQLite's online `.backup` command against the volume (safe while `token-server` is running in WAL mode; `/data` must be mounted read-write so `sqlite3` can coordinate with the WAL/SHM files), or stop `token-server` before copying `/data` directly:
  ```bash
  # Online consistent SQLite backup (adjust volume name if Compose project differs; check `docker volume ls`)
  mkdir -p /opt/zenha/backups
  docker run --rm -v zenha_zenha_data:/data -v /opt/zenha/backups:/backup alpine \
    sh -c 'apk add --no-cache sqlite >/dev/null && sqlite3 /data/chat.db ".backup /backup/chat-$(date -u +%Y%m%dT%H%M%SZ).db"'
  ```
- **Deleting all saved messages:** Stop `token-server`, remove the database files from the volume, and start it again:
  ```bash
  cd /opt/zenha
  docker compose stop token-server
  docker run --rm -v zenha_zenha_data:/data alpine rm -f /data/chat.db /data/chat.db-wal /data/chat.db-shm
  docker compose up -d token-server
  ```
