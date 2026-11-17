# Regras de `zenha-client`: interface `egui`, thread da UI e estado (UI)

Complementa [general.md](general.md). Escopo: `crates/zenha-client/`.

## UI-01: `zenha-client` nunca importa `livekit` nem `libwebrtc`

- `zenha-client` depende apenas de `zenha-core` e `zenha-protocol` para comunicação de domínio.
- Toda interação com a sessão passa pelo trait `SessionPort` (`ui_model.rs`), implementado por `CoreSession` (produção) e `DemoSession` (`--demo`), trocando `UiCommand` e `UiEvent`.

## UI-02: A thread da UI (`fn ui`) nunca bloqueia nem faz I/O

- Dentro de `App::ui` e de qualquer função em `views/*.rs` ou `state/` chamada no ciclo de quadro:
  - proibido fazer chamadas HTTP (`reqwest`), abrir/salvar arquivos pesados, consultar o Windows Credential Manager (`keyring`) de forma síncrona ou esperar locks disputados;
  - leituras de canais (`session.try_recv()`, `save_results.try_recv()`, `update_rx`, `forwarded_links`, `global_keys.events()`, `tray.actions()`) usam sempre `try_recv()`, nunca `recv()` bloqueante;
  - operações lentas do SO (`keyring`, diálogo `rfd`, checagem de atualização e cópia/download de arquivos) rodam em thread separada devolvendo o resultado via `mpsc` + `ctx.request_repaint()`.

## UI-03: Estado de tela em `enum Screen` e transições concentradas em `state/`

- A tela ativa fica em `enum Screen` (`state/mod.rs`).
- As views em `crates/zenha-client/src/views/` apenas desenham o estado e emitem `UserAction`, sem mutar estado de sessão nem enviar `UiCommand` diretamente por fora de `AppState`.
- Transições de tela, reação a `UiEvent` e geração de efeitos (`UiCommand`, `CredentialOp`, persistência de `Settings`) ficam em `state/` (testável sem abrir janela `eframe`).

## UI-04: Convenção do `eframe` 0.37 (`fn ui` com `&mut egui::Ui`)

- `impl eframe::App for App` implementa `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)`.
- Painéis e views recebem `ui: &mut egui::Ui` (acessando `ui.ctx()` só quando precisarem de recursos globais como texturas ou estilo), em vez de criar janelas soltas em `ctx`.

## UI-05: Textos de interface em português do Brasil

- Rótulos, botões, tooltips, banners, mensagens de erro (`describe(&SessionError)` em `core_session/`) e menu da bandeja (`platform/tray.rs`) ficam em português do Brasil claro, com acentuação correta.
- Mensagens técnicas cruas de bibliotecas (`reqwest`, `livekit`, `io::Error`) são traduzidas antes de aparecer em `UiEvent::Failed` ou nos banners.

## UI-06: Vídeo e imagens do chat com atualização sob demanda

- O `VideoRenderer` (`views/share.rs`) só faz upload para textura quando `FrameSlot::take_if_newer(&mut seen_generation)` devolve quadro novo.
- Imagens recebidas no chat (`chat_images.rs`) validam extensão/MIME (`is_image_mime`), decodificam com limite de tamanho/dimensão fora do caminho crítico e mantêm `TextureHandle` em cache por transferência, sem recriar textura a cada frame.

## UI-07: Links `zenha://` e IPC de instância única validados

- Argumentos CLI (`cli.rs`) e URIs `zenha://` recebidas pelo named pipe de instância única (`single_instance.rs`) são tratados como entrada não confiável:
  - o pipe limita a leitura a `MAX_MESSAGE` bytes via `.take(...)`;
  - o parser valida esquema, servidor, sala e código de convite antes de preencher `LoginForm` ou iniciar conexão.

## UI-08: Recursos nativos do Windows com fallback gracioso

- Integrações específicas de Windows (`global-hotkey`, `tray-icon`, `keyring`, `rfd`) ficam atrás de `#[cfg(windows)]` com stubs leves em `#[cfg(not(windows))]` para permitir `cargo test` e `cargo clippy` em qualquer ambiente.
- Falha ao registrar atalho global (`GlobalKeys`), ícone da bandeja (`Tray`) ou Credential Manager emite `tracing::warn!` e mantém o aplicativo funcional sem abortar a inicialização.

## UI-09: Paridade do modo `--demo` (`DemoSession`)

- Mudanças em `UiCommand`, `UiEvent` ou nos fluxos visuais de sala, chat, compartilhamento e arquivos devem manter `DemoSession` (`demo/`) coerente com `CoreSession`, pois `cargo client -- --demo` é o meio padrão de validação visual offline.
