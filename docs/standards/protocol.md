# Regras de `rooms-protocol`: contrato JSON e validação (PROTO)

Complementa [general.md](general.md). Escopo: `crates/rooms-protocol/`.

## PROTO-01: Crate enxuta: só `serde` e `thiserror`, sem I/O nem async

- `rooms-protocol` depende apenas de `serde` e `thiserror` em `[dependencies]`, sem `tokio`, `reqwest`, `axum`, `rusqlite` ou `livekit`, e sem acesso a rede, disco ou relógio do sistema.
- Contém apenas estruturas de dados serializáveis, constantes de limite (`MAX_*`) e funções puras de validação.

## PROTO-02: Limites de tamanho (`MAX_*`) centralizados e validados nas duas pontas

- Todo limite de tamanho ou página do protocolo (`MAX_DISPLAY_NAME_LEN`, `MAX_ROOM_NAME_LEN`, `MAX_CLIENT_ID_LEN`, `MAX_CHAT_MESSAGE_LEN`, `MAX_FILE_BYTES`, `MAX_FILE_NAME_LEN`, `MAX_CHAT_ID_LEN`, `CHAT_HISTORY_PAGE`) mora em `crates/rooms-protocol/src/lib.rs`.
- As funções `validate_*` (`validate_display_name`, `validate_room_name`, `validate_client_id`, `validate_chat_text`, `validate_chat_id`) são chamadas tanto no cliente (`rooms-client` / `rooms-core`) antes de enviar quanto no servidor (`rooms-token-server`) ao receber.

## PROTO-03: Mudança de contrato atualiza cliente e servidor no mesmo diff

- Alterar nome, tipo ou obrigatoriedade de campo em `TokenRequest`, `TokenResponse`, `RoomsResponse`, `ChatMessage`, `SaveChatRequest` ou `ChatHistoryResponse` quebra o protocolo HTTP/JSON.
- Atualize `rooms-protocol`, `rooms-core`, `rooms-client` e `rooms-token-server` juntos. Campos opcionais compatíveis com mensagens antigas usam `#[serde(default, ...)]` (como `ChatMessage::saved`).

## PROTO-04: Todo tipo serializável novo ganha teste de round-trip JSON

- Cada struct ou enum adicionado ou alterado em `rooms-protocol` precisa de teste unitário serializando com `serde_json::to_string` e desserializando com `serde_json::from_str`, verificando `assert_eq!` e os campos esperados.

## PROTO-05: Validação de strings conta caracteres Unicode, apara bordas e proíbe caracteres de controle

- Validações de texto livre (`display_name`, `chat_text`) aplicam `.trim()` antes de checar vazio, medem comprimento por `.chars().count()` (para não penalizar acentos) e rejeitam `char::is_control` (exceto `\n` onde permitido no chat).
- Identificadores (`room_name`, `client_id`, `chat_id`) usam allowlist explícita de caracteres ASCII permitidos.
