//! State transition for each `UserAction`.

use zenha_core::AdminAction;

use crate::platform::hotkeys::HotkeyAction;
use crate::ui_model::UiCommand;

use super::audio::update_participant_volume;
use super::{
    AdminDialog, AppState, FULL_VOLUME, LocalShare, RoomState, Screen, SecretText, UserAction,
    room_display_title,
};

impl AppState {
    pub fn handle_action(&mut self, action: UserAction) -> Vec<UiCommand> {
        match action {
            UserAction::SubmitOnboarding => self.submit_onboarding(),
            UserAction::ToggleInviteLink => {
                self.onboarding.link_open = !self.onboarding.link_open;
                self.onboarding.link_error = None;
                Vec::new()
            }
            UserAction::PasteInviteLink(text) => {
                self.paste_invite_link(text);
                Vec::new()
            }
            UserAction::JoinRoom(room) => self.join_room(room),
            UserAction::SubmitRoomPassword => self.submit_room_password(),
            UserAction::CancelRoomPassword => {
                self.room_password = None;
                Vec::new()
            }
            UserAction::CancelConnect => self.cancel_connect(),
            UserAction::ChangeServer => self.change_server(),
            UserAction::SaveDisplayName => self.save_display_name(),
            UserAction::PickAvatar => Vec::new(),
            UserAction::SetAvatar(path) => {
                self.avatar_notice = None;
                vec![UiCommand::SetAvatar { path }]
            }
            UserAction::RemoveAvatar => {
                self.avatar_notice = None;
                if self.avatar_version.is_some() {
                    vec![UiCommand::ClearAvatar]
                } else {
                    Vec::new()
                }
            }
            UserAction::CopyInviteLink => {
                self.invite_copied = true;
                Vec::new()
            }
            UserAction::Leave => self.leave(),
            UserAction::SwitchRoom(room) => self.switch_room(room),
            UserAction::ToggleMute => self.toggle_mute(),
            UserAction::ToggleDeafen => self.toggle_deafen(),
            UserAction::SendChat => self.with_room(RoomState::send_chat),
            UserAction::LoadOlderChat => self.with_room(RoomState::load_older_chat),
            UserAction::DeleteChat(id) => {
                let is_admin = self.admin.active;
                self.with_room(|room| room.delete_chat(id, is_admin))
            }
            UserAction::AttachFile(path) => self.with_room(|_| vec![UiCommand::SendFile { path }]),
            UserAction::StagePastedImage(image) => {
                if let Screen::InRoom(room) = &mut self.screen {
                    if let Some(previous) = room.chat.pending_paste.replace(image)
                        && room
                            .chat
                            .pending_paste
                            .as_ref()
                            .is_some_and(|current| current.path != previous.path)
                    {
                        self.pending_paste_deletes.push(previous.path);
                    }
                } else {
                    self.pending_paste_deletes.push(image.path);
                }
                Vec::new()
            }
            UserAction::SendPastedImage => self.with_room(|room| {
                room.chat
                    .pending_paste
                    .take()
                    .map_or_else(Vec::new, |pending| {
                        vec![UiCommand::SendFile { path: pending.path }]
                    })
            }),
            UserAction::CancelPastedImage => {
                self.discard_pending_paste();
                Vec::new()
            }
            UserAction::PickFile
            | UserAction::SaveReceivedFile { .. }
            | UserAction::OpenImageExternally(_) => Vec::new(),
            UserAction::OpenImageViewer(id) => self.with_room(|room| {
                room.chat.open_image(&id);
                Vec::new()
            }),
            UserAction::CloseImageViewer => self.with_room(|room| {
                room.chat.close_image();
                Vec::new()
            }),
            UserAction::DismissChatNotice => self.with_room(|room| {
                room.chat.notice = None;
                Vec::new()
            }),
            UserAction::OpenSettings => {
                self.audio.open = true;
                self.name_draft.clone_from(&self.form.display_name);
                self.invite_copied = false;
                self.admin.notice = None;
                let mut commands = vec![UiCommand::RefreshAudioDevices];
                commands.extend(self.request_webhooks());
                commands
            }
            UserAction::CloseSettings => {
                self.audio.open = false;
                self.hotkeys.capturing = None;
                self.hotkeys.capture_error = None;
                self.invite_copied = false;
                self.admin.error = None;
                self.admin.notice = None;
                self.admin.webhooks.close_session();
                Vec::new()
            }
            UserAction::StartHotkeyCapture(action) => {
                self.hotkeys.capturing = Some(action);
                self.hotkeys.capture_error = None;
                Vec::new()
            }
            UserAction::CancelHotkeyCapture => {
                self.hotkeys.capturing = None;
                self.hotkeys.capture_error = None;
                Vec::new()
            }
            UserAction::RejectHotkey(message) => {
                self.hotkeys.capture_error = Some(message);
                Vec::new()
            }
            UserAction::SetHotkey(action, binding) => {
                self.set_hotkey(action, binding);
                Vec::new()
            }
            UserAction::HotkeyPressed(action) => {
                if self.hotkeys.capturing.is_some() {
                    return Vec::new();
                }
                match action {
                    HotkeyAction::ToggleMute => self.toggle_mute(),
                    HotkeyAction::ToggleDeafen => self.toggle_deafen(),
                    HotkeyAction::PushToTalk => Vec::new(),
                }
            }
            UserAction::RefreshAudioDevices => vec![UiCommand::RefreshAudioDevices],
            UserAction::SelectInputDevice(id) => self.select_input(id),
            UserAction::SelectOutputDevice(id) => self.select_output(id),
            UserAction::SelectNoiseMode(mode) => self.select_noise(mode),
            UserAction::SetEchoCancellation(enabled) => self.set_echo_cancellation(enabled),
            UserAction::SelectVoiceMode(mode) => self.select_voice_mode(mode),
            UserAction::SetStreamVolume(percent) => {
                let percent = percent.min(FULL_VOLUME);
                if self.audio.stream_volume == percent {
                    return Vec::new();
                }
                self.audio.stream_volume = percent;
                vec![UiCommand::SetStreamVolume(percent)]
            }
            UserAction::SetParticipantVolume { identity, percent } => {
                let Some(applied) = update_participant_volume(
                    &mut self.audio.participant_volumes,
                    identity.clone(),
                    percent,
                ) else {
                    return Vec::new();
                };
                vec![UiCommand::SetParticipantVolume {
                    identity,
                    percent: applied,
                }]
            }
            UserAction::SetNoiseGate(threshold) => {
                if self.audio.noise_gate == threshold {
                    return Vec::new();
                }
                self.audio.noise_gate = threshold;
                vec![UiCommand::SetNoiseGate(threshold)]
            }
            UserAction::SaveAudioSettings => {
                self.pending_save = Some(self.settings_snapshot());
                Vec::new()
            }
            UserAction::SetCloseToTray(enabled) => {
                self.close_to_tray = enabled;
                self.pending_save = Some(self.settings_snapshot());
                Vec::new()
            }
            UserAction::SetRoomSounds(enabled) => {
                self.room_sounds = enabled;
                self.pending_save = Some(self.settings_snapshot());
                Vec::new()
            }
            UserAction::SetMutedOnJoin(enabled) => {
                self.muted_on_join = enabled;
                if matches!(self.screen, Screen::Lobby) && enabled && !self.lobby_voice.deafened {
                    self.lobby_voice.muted = true;
                }
                self.pending_save = Some(self.settings_snapshot());
                Vec::new()
            }
            UserAction::OpenSharePicker => self.open_picker(),
            UserAction::CloseSharePicker => {
                self.picker.close();
                vec![UiCommand::CancelCaptureTargets]
            }
            UserAction::RefreshCaptureTargets => {
                self.picker.reset_loading();
                vec![UiCommand::RefreshCaptureTargets]
            }
            UserAction::SelectCaptureTarget(id) => {
                if self.picker.targets.iter().any(|t| t.id == id) {
                    self.picker.selected = Some(id);
                }
                Vec::new()
            }
            UserAction::SelectSharePreset(preset) => {
                self.picker.preset = preset;
                Vec::new()
            }
            UserAction::StartShare => self.start_share(),
            UserAction::StopShare => self.with_room(|room| {
                if room.share.local == LocalShare::Active {
                    vec![UiCommand::StopScreenShare]
                } else {
                    Vec::new()
                }
            }),
            UserAction::ToggleSharePreviewCollapsed => {
                self.with_room(|room| {
                    room.share.toggle_preview_collapsed();
                    Vec::new()
                });
                Vec::new()
            }
            UserAction::DismissShareError => {
                self.with_room(|room| {
                    room.share.error = None;
                    Vec::new()
                });
                Vec::new()
            }
            UserAction::Watch(identity) => self.with_room(|room| room.share.watch(identity)),
            UserAction::StopWatching => self.with_room(|room| room.share.stop_watching()),
            UserAction::ToggleFullscreen => {
                self.with_room(|room| {
                    room.share.toggle_fullscreen();
                    Vec::new()
                });
                Vec::new()
            }
            UserAction::ToggleChat => {
                if let Screen::InRoom(state) = &mut self.screen {
                    state.chat.open = !state.chat.open;
                    if state.chat.open {
                        state.chat.unread = 0;
                    }
                }
                Vec::new()
            }
            UserAction::StartUpdate => {
                self.start_update();
                Vec::new()
            }
            UserAction::ConfirmUpdate => {
                self.confirm_update();
                Vec::new()
            }
            UserAction::CancelUpdateConfirm => {
                self.cancel_update_confirm();
                Vec::new()
            }
            UserAction::CancelUpdateDownload => {
                self.cancel_update_download();
                Vec::new()
            }
            UserAction::RetryUpdate => {
                self.retry_update();
                Vec::new()
            }
            UserAction::DismissUpdate => {
                self.dismiss_update();
                Vec::new()
            }
            UserAction::DismissLobbyNotice => {
                self.lobby_notice = None;
                Vec::new()
            }
            UserAction::AdminLogin => self.admin_login(),
            UserAction::AdminLogout => self.admin_logout(),
            UserAction::RotateInvite => self.rotate_invite(),
            UserAction::CreateWebhook => self.create_webhook(),
            UserAction::OpenRevokeWebhook(id) => {
                self.open_revoke_webhook(id);
                Vec::new()
            }
            UserAction::DismissCreatedWebhook => {
                self.admin.webhooks.created = None;
                Vec::new()
            }
            UserAction::DismissAdminNotice => {
                self.admin.error = None;
                self.admin.notice = None;
                Vec::new()
            }
            UserAction::OpenCreateRoom => {
                if self.admin.active {
                    self.admin.dialog = Some(AdminDialog::CreateRoom {
                        title: String::new(),
                        password: SecretText::default(),
                        parent: None,
                        error: None,
                    });
                }
                Vec::new()
            }
            UserAction::OpenEditRoom(id) => {
                if self.admin.active {
                    self.open_edit_room(id);
                }
                Vec::new()
            }
            UserAction::OpenDeleteRoom(id) => {
                if self.admin.active && self.rooms.iter().any(|r| r.name == id) {
                    let title = room_display_title(&self.rooms, &id).to_owned();
                    self.admin.dialog = Some(AdminDialog::DeleteRoom { id, title });
                }
                Vec::new()
            }
            UserAction::MoveRoomUp(id) => self.move_room(&id, -1),
            UserAction::MoveRoomDown(id) => self.move_room(&id, 1),
            UserAction::ConfirmAdminDialog => self.confirm_admin_dialog(),
            UserAction::CancelAdminDialog => {
                self.admin.dialog = None;
                Vec::new()
            }
            UserAction::AdminMuteParticipant { identity } => {
                let Some((room, _)) = self.remote_target_room(&identity) else {
                    return Vec::new();
                };
                vec![UiCommand::Admin(AdminAction::MuteParticipant {
                    room,
                    identity,
                })]
            }
            UserAction::AdminStopShareParticipant { identity } => {
                let Some((room, _)) = self.remote_target_room(&identity) else {
                    return Vec::new();
                };
                let is_sharing = matches!(
                    &self.screen,
                    Screen::InRoom(state) if state.share.is_sharing_remote(&identity)
                );
                if is_sharing {
                    vec![UiCommand::Admin(AdminAction::StopShareParticipant {
                        room,
                        identity,
                    })]
                } else {
                    Vec::new()
                }
            }
            UserAction::AdminMoveParticipant { identity, to_room } => {
                let Some((room, _)) = self.remote_target_room(&identity) else {
                    return Vec::new();
                };
                if to_room.trim().is_empty() || to_room == room {
                    return Vec::new();
                }
                vec![UiCommand::Admin(AdminAction::MoveParticipant {
                    room,
                    identity,
                    to_room,
                })]
            }
            UserAction::OpenKickParticipant { identity } => {
                let Some((room, name)) = self.remote_target_room(&identity) else {
                    return Vec::new();
                };
                self.admin.dialog = Some(AdminDialog::KickParticipant {
                    room,
                    identity,
                    name,
                });
                Vec::new()
            }
        }
    }
}
