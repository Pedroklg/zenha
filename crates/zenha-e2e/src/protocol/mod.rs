//! JSON line protocol between orchestrator and bots: commands on stdin, events on stdout.

mod command;
mod event;
#[cfg(test)]
mod tests;

pub use command::{BotCommand, NoiseKind, SharePresetKind, parse_command};
pub use event::{
    BotEvent, DeviceInfo, ErrorInfo, ParticipantInfo, RoomInfo, Stamped, TargetInfo, decode_event,
    encode_event, error_kind,
};
