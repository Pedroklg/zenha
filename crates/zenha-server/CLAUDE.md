# zenha-server

Servidor HTTP (`axum`) que troca código de convite por token do LiveKit e guarda as mensagens de chat marcadas como salvas.

- Estado: `Config` e o SQLite de mensagens salvas (`ZENHA_CHAT_DB`, `store/`). A `Config` é lida de variáveis de ambiente (`ZENHA_INVITE_CODE_HASH` com hash Argon2, ou `ZENHA_INVITE_CODE` em texto com aviso na partida; veja `.env.example`).
- Subcomando `zenha-server hash-password` lê a senha da entrada padrão e imprime o hash PHC sem ler `.env` nem subir o servidor.
- Segredos nunca aparecem em log nem em resposta de erro.
- Verifica o código de convite com `verify_password` do Argon2 (quando `ZENHA_INVITE_CODE_HASH` está definido) ou em tempo constante (no fallback em texto).
- Emite tokens com `identity = client_id` e `name = display_name.trim()`, validade curta e permissão só para a sala pedida.
- Todo handler tem teste com `tower::ServiceExt::oneshot`, sem abrir porta.
- Rotas de chat autenticam pelo JWT da sala (`Authorization: Bearer`); autor vem do token, nunca do corpo.
