# rooms-e2e

Harness de testes ponta a ponta: `rooms-e2e bot` (participante sem janela) e `rooms-e2e run` (orquestrador).

- Só depende de `rooms-core` e `rooms-protocol`; nunca importa `livekit`.
- Código do Windows (`loopback.rs`, `wasapi`) fica atrás de `#[cfg(windows)]`, pequeno e isolado.
- Lógica pura (Goertzel, decisão do tom, protocolo, relatório) não depende de plataforma e tem testes que rodam no Linux.
- Nunca lê `.env`: o servidor de tokens recebe variáveis de ambiente explícitas e roda fora do repositório.
- Os limiares de `detect.rs` estão justificados nos comentários; mudar um exige conferir os testes de `detect` e `dsp`.
- O relatório sai em português; logs e identificadores em inglês.
