# rooms-core

Sessão, áudio, captura e vídeo. Única crate que conhece o SDK do LiveKit.

- API pública: `Command`, `Event`, `SessionError` e os tipos próprios. Nada de `livekit::*` em assinatura pública.
- A sessão roda em tarefas Tokio; a interface só troca mensagens por canais.
- Captura de tela implementa o trait `FrameSource`. Frames atrasados são descartados, nunca enfileirados.
- Único lugar onde `unsafe` é aceito, sempre com `#[expect(unsafe_code)]` e `SAFETY:`.
- Código específico de Windows (`wasapi`, `windows-capture`) fica em módulos `#[cfg(windows)]`.
- Antes de chamar o SDK, leia a assinatura na versão fixada; a API muda entre releases.
- `LiveKitBackendOptions` (microfone sintético, alvo "Padrão de teste") existe só para o `rooms-e2e`; os valores padrão têm de reproduzir o cliente.
