# 0002. Interface em egui com wgpu

- Status: aceita
- Data: 2026-11-09

## Contexto

A mídia roda em Rust nativo e os frames de vídeo decodificados precisam aparecer na janela. Só Windows é alvo.

## Decisão

Usar `eframe`/`egui` com `wgpu`, desenhando o vídeo como textura.

## Alternativas descartadas

- **Tauri com React e mídia em Rust.** Mostrar vídeo nativo exige uma janela filha sincronizada sob a webview, ou empurrar frames para a webview, caminho sem medições públicas a 1080p60.
- **Tauri ou Electron com o SDK JavaScript.** Menor esforço, mas a mídia sai do Rust.
- **iced.** Arquitetura mais limpa, mas o widget de vídeo teria que ser escrito do zero.
- **GPUI.** Documentação escassa e Windows fora da lista de suporte do README.

## Consequências

- Existe um cliente de referência oficial com a mesma combinação (`rust-dev-client`).
- O visual é mais simples que o de uma interface web.
- `egui` é pré-1.0 e muda a API a cada release; a versão fica fixada.
