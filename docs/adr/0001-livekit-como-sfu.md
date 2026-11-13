# 0001. LiveKit como SFU, com cliente no SDK Rust

- Status: aceita
- Data: 2026-11-09

## Contexto

O app atende 8 a 10 pessoas com voz e transmissão de tela. Em malha P2P, quem transmite codifica e envia o vídeo uma vez por espectador, o que não cabe em upload doméstico. O objetivo é escrever todo o código próprio em Rust.

## Decisão

Usar o `livekit-server` como binário pronto e o SDK Rust (`livekit`, `libwebrtc`) no cliente, isolado na crate `rooms-core`.

## Alternativas descartadas

- **SFU próprio com `str0m`.** O `str0m` cobre só o transporte; codecs, jitter buffer, cancelamento de eco e captura ficariam por conta do projeto.
- **SFU pronto escrito em Rust.** Em novembro de 2026 nenhum é maduro e tem cliente Rust nativo (`webrtc-rs/sfu` pré-produção, `rheomesh` só com cliente TypeScript, `atm0s-media-server` parado).
- **MoQ ou iroh-live.** Prévias técnicas; o iroh-live não foi rodado em Windows pelos mantenedores.
- **SDK JavaScript em webview.** Mais rápido, mas tira a mídia do Rust.

## Consequências

- O servidor é escrito em Go, mas é só operado, nunca modificado.
- O cliente fala o protocolo do LiveKit; trocar de SFU exige refazer a camada de rede de `rooms-core`.
- O SDK é 0.x e quebra a API com frequência: versões exatas e atualização deliberada.
