# Deploy

Ponto de partida para a T20. **Nada aqui foi testado ainda**; confira cada chave na documentação do LiveKit antes de usar.

- `livekit.example.yaml`: copie para `livekit.prod.yaml` (ignorado pelo git) e preencha chave e segredo.
- `docker-compose.yml`: LiveKit em rede do host, servidor de tokens e Caddy para TLS.
- `Caddyfile`: troque os domínios.

Portas a abrir: 443/TCP, 7881/TCP, 7882/UDP e, se usar o TURN embutido, 3478/UDP.

Gerar chave e segredo de produção: `livekit-server generate-keys`.
Gerar hash da senha do servidor (`ROOMS_INVITE_CODE_HASH`): `echo sua-senha | rooms-token-server hash-password`.
