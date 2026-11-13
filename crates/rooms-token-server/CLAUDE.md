# rooms-token-server

Servidor HTTP (`axum`) que troca código de convite por token do LiveKit.

- Sem estado além da `Config`, lida de variáveis de ambiente (veja `.env.example`).
- Segredos nunca aparecem em log nem em resposta de erro.
- Compare o código de convite em tempo constante.
- Tokens com validade curta e permissão só para a sala pedida.
- Todo handler tem teste com `tower::ServiceExt::oneshot`, sem abrir porta.
