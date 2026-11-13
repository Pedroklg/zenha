# rooms-token-server

Servidor HTTP (`axum`) que troca código de convite por token do LiveKit.

- Sem estado além da `Config`, lida de variáveis de ambiente (`ROOMS_INVITE_CODE_HASH` com hash Argon2, ou `ROOMS_INVITE_CODE` em texto com aviso na partida; veja `.env.example`).
- Subcomando `rooms-token-server hash-password` lê a senha da entrada padrão e imprime o hash PHC sem ler `.env` nem subir o servidor.
- Segredos nunca aparecem em log nem em resposta de erro.
- Verifica o código de convite com `verify_password` do Argon2 (quando `ROOMS_INVITE_CODE_HASH` está definido) ou em tempo constante (no fallback em texto).
- Emite tokens com `identity = client_id` e `name = display_name.trim()`, validade curta e permissão só para a sala pedida.
- Todo handler tem teste com `tower::ServiceExt::oneshot`, sem abrir porta.
