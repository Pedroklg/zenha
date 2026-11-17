//! Protocolo de linhas JSON entre o orquestrador e os bots: comandos no stdin, eventos no stdout.

mod command;
mod event;
#[cfg(test)]
mod tests;

pub use command::{BotCommand, NoiseKind, parse_command};
pub use event::{
    BotEvent, DeviceInfo, ErrorInfo, ParticipantInfo, RoomInfo, Stamped, TargetInfo, decode_event,
    encode_event, error_kind,
};
