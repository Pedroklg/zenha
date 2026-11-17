//! Conversion of UI commands into core commands.

use zenha_core::{Command, NoiseSuppression, SharePreset};

use crate::platform::settings::NoiseMode;
use crate::ui_model::{SharePresetView, UiCommand};

pub(super) fn to_core(command: UiCommand) -> Command {
    match command {
        UiCommand::Join { room } => Command::Join { room },
        UiCommand::UnlockRoom { room, password } => Command::UnlockRoom { room, password },
        UiCommand::Leave => Command::Leave,
        UiCommand::SetMuted(muted) => Command::SetMuted(muted),
        UiCommand::SetDeafened(deafened) => Command::SetDeafened(deafened),
        UiCommand::SetDisplayName { name } => Command::SetDisplayName { name },
        UiCommand::SetAvatar { path } => Command::SetAvatar { path },
        UiCommand::ClearAvatar => Command::ClearAvatar,
        UiCommand::SendChat { text, save } => Command::SendChat { text, save },
        UiCommand::LoadChatHistory { before } => Command::LoadChatHistory { before },
        UiCommand::DeleteChat { id } => Command::DeleteChat { id },
        UiCommand::SendFile { path } => Command::SendFile { path },
        UiCommand::RefreshRooms => Command::RefreshRooms,
        UiCommand::RefreshAudioDevices => Command::RefreshAudioDevices,
        UiCommand::SetInputDevice(id) => Command::SetInputDevice(id),
        UiCommand::SetOutputDevice(id) => Command::SetOutputDevice(id),
        UiCommand::SetStreamVolume(percent) => Command::SetStreamVolume(percent),
        UiCommand::SetParticipantVolume { identity, percent } => {
            Command::SetParticipantVolume { identity, percent }
        }
        UiCommand::SetNoiseGate(threshold) => Command::SetNoiseGate(threshold),
        UiCommand::SetNoiseSuppression(mode) => Command::SetNoiseSuppression(match mode {
            NoiseMode::Off => NoiseSuppression::Off,
            NoiseMode::Standard => NoiseSuppression::Standard,
            NoiseMode::Enhanced => NoiseSuppression::Enhanced,
            NoiseMode::Maximum => NoiseSuppression::Maximum,
        }),
        UiCommand::RefreshCaptureTargets => Command::RefreshCaptureTargets,
        UiCommand::CancelCaptureTargets => Command::CancelCaptureTargets,
        UiCommand::StartScreenShare { target_id, preset } => Command::StartScreenShare {
            target_id,
            preset: to_core_preset(preset),
        },
        UiCommand::StopScreenShare => Command::StopScreenShare,
        UiCommand::WatchScreenShare { identity } => Command::WatchScreenShare { identity },
        UiCommand::StopWatching => Command::StopWatching,
        UiCommand::SetUiVisible(visible) => Command::SetUiVisible(visible),
        UiCommand::PlayCue(cue) => Command::PlayCue(cue),
        UiCommand::AdminLogin { password } => Command::AdminLogin { password },
        UiCommand::AdminLogout => Command::AdminLogout,
        UiCommand::Admin(action) => Command::Admin(action),
    }
}

fn to_core_preset(preset: SharePresetView) -> SharePreset {
    match preset {
        SharePresetView::P720F30 => SharePreset::P720F30,
        SharePresetView::P1080F30 => SharePreset::P1080F30,
        SharePresetView::P1080F60 => SharePreset::P1080F60,
    }
}
