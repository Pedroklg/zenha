#!/usr/bin/env bash
set -euo pipefail

ZENHA_DIR="/opt/zenha"
ENV_FILE="${ZENHA_DIR}/.env"
LIVEKIT_FILE="${ZENHA_DIR}/livekit.yaml"
COMPOSE_FILE="${ZENHA_DIR}/docker-compose.yml"
CADDY_FILE="${ZENHA_DIR}/Caddyfile"
LIVEKIT_IMAGE="livekit/livekit-server:v1.13.11"

for arg in "$@"; do
  case "${arg}" in
    TOKEN_DOMAIN=*) TOKEN_DOMAIN="${arg#*=}" ;;
    RTC_DOMAIN=*) RTC_DOMAIN="${arg#*=}" ;;
    ZENHA_IMAGE=*) ZENHA_IMAGE="${arg#*=}" ;;
    ROOMS_INVITE_CODE_HASH=*) ROOMS_INVITE_CODE_HASH="${arg#*=}" ;;
    INVITE_PASSWORD=*) INVITE_PASSWORD="${arg#*=}" ;;
    ROOMS_ADMIN_CODE_HASH=*) ROOMS_ADMIN_CODE_HASH="${arg#*=}" ;;
    ADMIN_PASSWORD=*) ADMIN_PASSWORD="${arg#*=}" ;;
    ROOMS_ROOM_LIST=*) ROOMS_ROOM_LIST="${arg#*=}" ;;
    ROOMS_TOKEN_TTL_SECS=*) ROOMS_TOKEN_TTL_SECS="${arg#*=}" ;;
    NODE_IP=*) NODE_IP="${arg#*=}" ;;
    --token-domain=*) TOKEN_DOMAIN="${arg#*=}" ;;
    --rtc-domain=*) RTC_DOMAIN="${arg#*=}" ;;
    --image=*) ZENHA_IMAGE="${arg#*=}" ;;
    --invite-hash=*) ROOMS_INVITE_CODE_HASH="${arg#*=}" ;;
    --invite-password=*) INVITE_PASSWORD="${arg#*=}" ;;
    --admin-hash=*) ROOMS_ADMIN_CODE_HASH="${arg#*=}" ;;
    --admin-password=*) ADMIN_PASSWORD="${arg#*=}" ;;
    --node-ip=*) NODE_IP="${arg#*=}" ;;
    *)
      echo "Unknown argument: ${arg}" >&2
      echo "Usage: $0 [TOKEN_DOMAIN=...] [RTC_DOMAIN=...] [ZENHA_IMAGE=...] [ROOMS_INVITE_CODE_HASH=...] [INVITE_PASSWORD=...] [ROOMS_ADMIN_CODE_HASH=...] [ADMIN_PASSWORD=...] [NODE_IP=...]" >&2
      exit 1
      ;;
  esac
done

if [[ "${EUID}" -ne 0 ]]; then
  echo "error: setup-server.sh must run as root (use sudo or az vm run-command)" >&2
  exit 1
fi

for i in $(seq 1 60); do
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    break
  fi
  if [[ "${i}" -eq 60 ]]; then
    echo "error: docker daemon is not ready (check cloud-init status)" >&2
    exit 1
  fi
  sleep 5
done

install -d -m 0750 -o root -g root "${ZENHA_DIR}"

read_existing_env() {
  local key="$1"
  if [[ -f "${ENV_FILE}" ]]; then
    local line
    line="$(grep -E "^${key}=" "${ENV_FILE}" | tail -n 1 || true)"
    if [[ -n "${line}" ]]; then
      local val="${line#*=}"
      val="${val#\'}"
      val="${val%\'}"
      val="${val#\"}"
      val="${val%\"}"
      printf '%s' "${val}"
    fi
  fi
}

TOKEN_DOMAIN="${TOKEN_DOMAIN:-$(read_existing_env TOKEN_DOMAIN)}"
RTC_DOMAIN="${RTC_DOMAIN:-$(read_existing_env RTC_DOMAIN)}"
ZENHA_IMAGE="${ZENHA_IMAGE:-$(read_existing_env ZENHA_IMAGE)}"
ROOMS_INVITE_CODE_HASH="${ROOMS_INVITE_CODE_HASH:-$(read_existing_env ROOMS_INVITE_CODE_HASH)}"
if [[ -z "${ROOMS_ADMIN_CODE_HASH:-}" && -z "${ADMIN_PASSWORD:-}" ]]; then
  ROOMS_ADMIN_CODE_HASH="$(read_existing_env ROOMS_ADMIN_CODE_HASH)"
fi
ROOMS_ADMIN_CODE_HASH="${ROOMS_ADMIN_CODE_HASH:-}"
LIVEKIT_API_KEY="${LIVEKIT_API_KEY:-$(read_existing_env LIVEKIT_API_KEY)}"
LIVEKIT_API_SECRET="${LIVEKIT_API_SECRET:-$(read_existing_env LIVEKIT_API_SECRET)}"
ROOMS_ROOM_LIST="${ROOMS_ROOM_LIST:-$(read_existing_env ROOMS_ROOM_LIST)}"
ROOMS_ROOM_LIST="${ROOMS_ROOM_LIST:-geral,jogos,afk}"
ROOMS_TOKEN_TTL_SECS="${ROOMS_TOKEN_TTL_SECS:-$(read_existing_env ROOMS_TOKEN_TTL_SECS)}"
ROOMS_TOKEN_TTL_SECS="${ROOMS_TOKEN_TTL_SECS:-3600}"
NODE_IP="${NODE_IP:-}"

if [[ -z "${TOKEN_DOMAIN}" ]] && [[ -t 0 ]]; then
  read -r -p "Token server domain (e.g. voice.example.com): " TOKEN_DOMAIN
fi
if [[ -z "${RTC_DOMAIN}" ]] && [[ -t 0 ]]; then
  read -r -p "LiveKit RTC domain (e.g. rtc.voice.example.com): " RTC_DOMAIN
fi
if [[ -z "${ZENHA_IMAGE}" ]] && [[ -t 0 ]]; then
  read -r -p "Token server GHCR image (e.g. ghcr.io/owner/zenha-token-server:v0.1.0): " ZENHA_IMAGE
fi

if [[ -z "${TOKEN_DOMAIN}" || -z "${RTC_DOMAIN}" || -z "${ZENHA_IMAGE}" ]]; then
  echo "error: TOKEN_DOMAIN, RTC_DOMAIN, and ZENHA_IMAGE are required" >&2
  exit 1
fi

if [[ -z "${ROOMS_INVITE_CODE_HASH}" ]]; then
  if [[ -n "${INVITE_PASSWORD:-}" ]]; then
    ROOMS_INVITE_CODE_HASH="$(printf '%s\n' "${INVITE_PASSWORD}" | docker run --rm -i "${ZENHA_IMAGE}" hash-password)"
  elif [[ -t 0 ]]; then
    read -r -s -p "Enter Argon2 PHC hash (\$argon2id\$...) or plaintext invite password: " USER_SECRET
    echo >&2
    if [[ "${USER_SECRET}" == \$argon2* ]]; then
      ROOMS_INVITE_CODE_HASH="${USER_SECRET}"
    elif [[ -n "${USER_SECRET}" ]]; then
      ROOMS_INVITE_CODE_HASH="$(printf '%s\n' "${USER_SECRET}" | docker run --rm -i "${ZENHA_IMAGE}" hash-password)"
    fi
  fi
fi

if [[ -z "${ROOMS_INVITE_CODE_HASH}" || "${ROOMS_INVITE_CODE_HASH}" != \$argon2* ]]; then
  echo "error: valid ROOMS_INVITE_CODE_HASH (\$argon2id\$...) or INVITE_PASSWORD is required" >&2
  exit 1
fi

if [[ -z "${ROOMS_ADMIN_CODE_HASH}" ]]; then
  if [[ -n "${ADMIN_PASSWORD:-}" ]]; then
    ROOMS_ADMIN_CODE_HASH="$(printf '%s\n' "${ADMIN_PASSWORD}" | docker run --rm -i "${ZENHA_IMAGE}" hash-password)"
  elif [[ -t 0 ]]; then
    read -r -s -p "Optional admin Argon2 PHC hash or plaintext password (Enter to skip): " ADMIN_SECRET
    echo >&2
    if [[ "${ADMIN_SECRET}" == \$argon2* ]]; then
      ROOMS_ADMIN_CODE_HASH="${ADMIN_SECRET}"
    elif [[ -n "${ADMIN_SECRET}" ]]; then
      ROOMS_ADMIN_CODE_HASH="$(printf '%s\n' "${ADMIN_SECRET}" | docker run --rm -i "${ZENHA_IMAGE}" hash-password)"
    fi
  fi
fi

if [[ -n "${ROOMS_ADMIN_CODE_HASH}" && "${ROOMS_ADMIN_CODE_HASH}" != \$argon2* ]]; then
  echo "error: ROOMS_ADMIN_CODE_HASH must be an Argon2 PHC hash (\$argon2id\$...)" >&2
  exit 1
fi

if [[ -z "${LIVEKIT_API_KEY}" || -z "${LIVEKIT_API_SECRET}" ]]; then
  KEY_OUTPUT="$(docker run --rm "${LIVEKIT_IMAGE}" generate-keys 2>/dev/null || true)"
  LIVEKIT_API_KEY="$(printf '%s\n' "${KEY_OUTPUT}" | awk '/API Key:/ {print $NF}')"
  LIVEKIT_API_SECRET="$(printf '%s\n' "${KEY_OUTPUT}" | awk '/API Secret:/ {print $NF}')"
  if [[ -z "${LIVEKIT_API_KEY}" || -z "${LIVEKIT_API_SECRET}" ]]; then
    LIVEKIT_API_KEY="API$(openssl rand -hex 6)"
    LIVEKIT_API_SECRET="$(openssl rand -base64 36 | tr -d '=+/\n' | cut -c1-40)"
  fi
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ -f "${SCRIPT_DIR}/docker-compose.yml" && "${SCRIPT_DIR}/docker-compose.yml" != "${COMPOSE_FILE}" ]]; then
  install -m 0644 -o root -g root "${SCRIPT_DIR}/docker-compose.yml" "${COMPOSE_FILE}"
else
  cat <<'EOF' >"${COMPOSE_FILE}"
services:
  livekit:
    image: livekit/livekit-server:v1.13.11
    command: --config /etc/livekit.yaml
    network_mode: host
    restart: unless-stopped
    volumes:
      - /opt/zenha/livekit.yaml:/etc/livekit.yaml:ro

  token-server:
    image: ${ZENHA_IMAGE:?set ZENHA_IMAGE in /opt/zenha/.env}
    network_mode: host
    restart: unless-stopped
    env_file:
      - /opt/zenha/.env
    environment:
      ROOMS_CHAT_DB: /var/lib/zenha/chat.db
    volumes:
      - zenha_data:/var/lib/zenha
    depends_on:
      - livekit

  caddy:
    image: caddy:2.11.9-alpine
    network_mode: host
    restart: unless-stopped
    env_file:
      - /opt/zenha/.env
    volumes:
      - /opt/zenha/Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
      - caddy_config:/config
    depends_on:
      - livekit
      - token-server

volumes:
  caddy_data:
  caddy_config:
  zenha_data:
EOF
  chmod 0644 "${COMPOSE_FILE}"
fi

if [[ -f "${SCRIPT_DIR}/Caddyfile" && "${SCRIPT_DIR}/Caddyfile" != "${CADDY_FILE}" ]]; then
  install -m 0644 -o root -g root "${SCRIPT_DIR}/Caddyfile" "${CADDY_FILE}"
else
  cat <<'EOF' >"${CADDY_FILE}"
{$TOKEN_DOMAIN} {
	reverse_proxy 127.0.0.1:8787
}

{$RTC_DOMAIN} {
	reverse_proxy 127.0.0.1:7880
}
EOF
  chmod 0644 "${CADDY_FILE}"
fi

umask 077

if [[ -n "${NODE_IP}" ]]; then
  RTC_IP_CONFIG="$(printf '  use_external_ip: false\n  node_ip: "%s"' "${NODE_IP}")"
else
  RTC_IP_CONFIG="  use_external_ip: true"
fi

cat <<EOF >"${LIVEKIT_FILE}"
port: 7880
rtc:
  tcp_port: 7881
  udp_port: 7882
${RTC_IP_CONFIG}
keys:
  ${LIVEKIT_API_KEY}: ${LIVEKIT_API_SECRET}
room:
  auto_create: true
  empty_timeout: 300
logging:
  level: info
EOF
chmod 0600 "${LIVEKIT_FILE}"
chown root:root "${LIVEKIT_FILE}"

cat <<EOF >"${ENV_FILE}"
ZENHA_IMAGE=${ZENHA_IMAGE}
TOKEN_DOMAIN=${TOKEN_DOMAIN}
RTC_DOMAIN=${RTC_DOMAIN}
ROOMS_BIND_ADDR=127.0.0.1:8787
ROOMS_INVITE_CODE_HASH='${ROOMS_INVITE_CODE_HASH}'
LIVEKIT_URL=wss://${RTC_DOMAIN}
LIVEKIT_API_KEY=${LIVEKIT_API_KEY}
LIVEKIT_API_SECRET=${LIVEKIT_API_SECRET}
ROOMS_TOKEN_TTL_SECS=${ROOMS_TOKEN_TTL_SECS}
ROOMS_ROOM_LIST=${ROOMS_ROOM_LIST}
RUST_LOG=info
EOF
if [[ -n "${ROOMS_ADMIN_CODE_HASH}" ]]; then
  printf "ROOMS_ADMIN_CODE_HASH='%s'\n" "${ROOMS_ADMIN_CODE_HASH}" >>"${ENV_FILE}"
fi
chmod 0600 "${ENV_FILE}"
chown root:root "${ENV_FILE}"

cd "${ZENHA_DIR}"
docker compose pull
docker compose up -d --remove-orphans

for i in $(seq 1 15); do
  if curl -fsS http://127.0.0.1:8787/health >/dev/null 2>&1; then
    echo "zenha stack is up and token-server health check passed"
    exit 0
  fi
  sleep 2
done

echo "error: token-server did not pass local health check on 127.0.0.1:8787" >&2
docker compose ps >&2
exit 1
