# Regras de `zenha-core`: LiveKit, `unsafe`, mídia e Tokio (CORE)

Complementa [general.md](general.md). Escopo: `crates/zenha-core/`.

## CORE-01: Isolamento absoluto do SDK LiveKit e `libwebrtc`

- `zenha-core` é a **única** crate do workspace autorizada a depender de `livekit` e `libwebrtc` (via `livekit`).
- Nenhum tipo, trait, erro ou re-export de `livekit::*` ou `libwebrtc::*` pode aparecer na API pública de `zenha-core` (`pub fn`, `pub struct`, `pub enum`, `pub trait`, `pub type` expostos em `lib.rs` ou módulos públicos).
- A fronteira pública usa exclusivamente `Command`, `Event`, `SessionError`, `SessionHandle`, `MediaBackend` e os tipos de domínio próprios (`Participant`, `AudioDevices`, `CaptureTarget`, `FrameSlot`, `VideoFrame`, `FileTransfer`).

## CORE-02: Conferência da assinatura do SDK na versão fixada

- Antes de chamar métodos de `livekit` ou `libwebrtc`, confira a assinatura real nos fontes da versão fixada no `Cargo.lock` (em `~/.cargo/registry/src/` ou `patches/`).
- Erros do SDK nunca chegam crus à UI: passam por sanitização (`sanitize_error` em `backend_support.rs`) ou viram variantes semânticas de `SessionError`.

## CORE-03: `unsafe` restrito, mínimo e documentado com `SAFETY:`

- Todo bloco `unsafe` em `zenha-core` exige:
  1. escopo mínimo (apenas a chamada FFI/Win32 necessária, nunca a função inteira);
  2. `#[expect(unsafe_code, reason = "...")]` acima da declaração/item que contém o `unsafe`;
  3. comentário `// SAFETY:` imediatamente acima do bloco `unsafe`, provando validade de ponteiros, handles (`HWND`), tamanhos e lifetimes mesmo diante de janelas fechadas ou entradas externas.
- Conversões de buffers em Rust puro usam indexação verificada (`chunks_exact`, `chunks_exact_mut`, `get`).

## CORE-04: Código de Windows isolado em `#[cfg(windows)]` e COM por thread

- Módulos que dependem de `wasapi`, `windows`, `deep_filter` ou `livekit` nativo ficam atrás de `#[cfg(windows)]` em `lib.rs` e `[target.'cfg(windows)'.dependencies]` no `Cargo.toml`, mantendo `zenha-core` compilável e testável com `NoMediaBackend`.
- Toda thread WASAPI (`default_device.rs`, `mic_capture.rs`, `share_audio.rs`, `audio_render.rs`) inicializa COM na própria thread (`initialize_mta()`) e garante `deinitialize()` em todos os caminhos de saída, sem vazar estado de apartment para threads do Tokio ou da UI.

## CORE-05: Captura atrás de `FrameSource` e descarte de atraso

- Capturadores de tela/janela implementam `FrameSource` (`list_targets`, `start`, `stop`).
- Frames atrasados nunca formam fila: `Pacer` (`capture.rs`) pula slots perdidos sem rajada ("burst") e `FrameSlot` (`video.rs`) mantém apenas o quadro mais recente ("latest frame wins").
- Em captura de áudio WASAPI (`mic_capture.rs`, `share_audio.rs`), excesso além de `MAX_BACKLOG_BYTES` descarta os quadros mais antigos alinhados em múltiplos de `FRAME_BYTES`.
- Antes de converter ou publicar um `BgraFrame`, valide `frame.is_consistent()`.

## CORE-06: Sem bloqueio nem `std::sync::MutexGuard` através de `.await`

- Nas tarefas Tokio (`session/`, `livekit_backend/`, `livekit_files.rs`, `token.rs`), nunca mantenha `std::sync::MutexGuard` ou `RwLock*Guard` vivo através de `.await`: copie o dado num bloco `{ ... }` síncrono curto ou use `tokio::sync::Mutex`.
- Nunca chame `std::thread::sleep`, `JoinHandle::join` demorado, I/O pesado síncrono ou inicialização COM direto na thread do runtime da sessão (`handle.rs`).

## CORE-07: Cancelamento limpo em `tokio::select!` e parada de threads em `Drop`

- Todo ramo de `tokio::select!` (`session/mod.rs`) precisa ser seguro ao cancelamento (cancellation-safe): futures de conexão ou backoff que guardam estado entre iterações permanecem em `Pin<Box<...>>` dentro de `Phase`.
- Structs que iniciam thread de captura/áudio ou tarefa em background (`Worker`, `MicCapture`, `ShareAudioCapture`, `OutputStream`, `SessionHandle`) implementam `Drop` sinalizando parada (`AtomicBool` / fechamento de canal) e aguardando encerramento ordenado.
- Ao sair do loop de sessão ou trocar de sala, execute `teardown().await`.

## CORE-08: `Mutex` e `RwLock` sem pânico por envenenamento (`PoisonError`)

- Nunca use `.lock().unwrap()` fora de `#[cfg(test)]`.
- Quando a invariante continua válida após pânico em outra thread, recupere com `.unwrap_or_else(PoisonError::into_inner)`.

## CORE-09: Caminho quente de áudio e vídeo sem alocações desnecessárias

- Em callbacks de áudio (10 ms / 100 Hz a 48 kHz) e loops de captura/conversão de vídeo (30–60 fps até 1080p), pré-aloque buffers fora do loop e reutilize-os por referência mutável (ex.: `DeepFilterDenoiser` em `deep_filter.rs`, reaproveitamento de `Vec<u8>` via `Arc::get_mut` no `FrameSlot`).

## CORE-10: Recebimento de arquivos com sanitização e limites estritos

- Todo arquivo recebido via byte stream (`livekit_files.rs`, `files.rs`) deve:
  1. validar `total_length` em `1..=MAX_FILE_BYTES` antes de abrir arquivo em disco e abortar se o stream exceder o tamanho anunciado;
  2. sanitizar o nome com `sanitize_file_name` (sem `/`, `\`, `..`, caracteres de controle ou nomes reservados do Windows `CON`, `PRN`, `AUX`, `NUL`, `COM1..9`, `LPT1..9`);
  3. limitar downloads simultâneos (`FileReceiver::try_acquire`);
  4. apagar o arquivo parcial `.part` em caso de falha ou cancelamento no `Drop` de `IncomingFile`.

## CORE-11: `LiveKitBackendOptions::default()` fiel ao cliente real

- Opções sintéticas para o E2E (`MicrophoneSource::Tone`, `TEST_PATTERN_TARGET_ID`) servem apenas ao `zenha-e2e`.
- `LiveKitBackendOptions::default()` e `LiveKitBackend::new()` mantêm sempre o comportamento de produção (`MicrophoneSource::System`, sem alvos sintéticos).

## CORE-12: Pontos de ligação completos para cada `Command` ou `Event` novo

- Ao adicionar ou alterar uma variante em `Command`, `Event` ou método em `MediaBackend`, atualize no mesmo diff os **8 pontos de ligação**:
  1. `crates/zenha-core/src/api.rs`
  2. `crates/zenha-core/src/backend.rs`
  3. `crates/zenha-core/src/session/`
  4. `crates/zenha-core/src/testing.rs`
  5. `crates/zenha-core/src/no_media.rs`
  6. `crates/zenha-core/src/livekit_backend/`
  7. `crates/zenha-client/src/` (`ui_model.rs`, `core_session.rs`, `demo.rs`, `state/`)
  8. `crates/zenha-e2e/src/protocol.rs`
