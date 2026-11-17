//! Transição de estado a partir de cada `UiEvent` vindo da sessão.

use zenha_core::RemovalReason;

use crate::ui_model::UiEvent;

use super::admin::upsert_admin_room;
use super::room::apply_room_event;
use super::{AdminDialog, AppState, CredentialOp, MicLevelView, Screen, room_display_title};

impl AppState {
    pub fn apply_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::RoomsUpdated(mut rooms) => {
                if !matches!(self.screen, Screen::Onboarding) {
                    for room in &mut rooms {
                        room.participants
                            .sort_by_cached_key(|name| name.to_lowercase());
                    }
                    self.rooms = rooms;
                    self.lobby_error = None;
                    self.prune_admin_dialog_for_rooms();
                    if let Some((server, password)) = self.pending_password_save.take() {
                        self.saved_credential = Some((server.clone(), password.clone()));
                        self.pending_credential_ops
                            .push(CredentialOp::Store { server, password });
                    }
                }
            }
            UiEvent::RoomsFailed { message } => {
                if self.room_password.as_ref().is_some_and(|d| d.busy) {
                    self.on_room_unlock_failed(message);
                } else if matches!(self.screen, Screen::Lobby | Screen::Connecting { .. }) {
                    self.lobby_error = Some(message);
                }
            }
            UiEvent::RoomUnlocked { room, key } => {
                let commands = self.on_room_unlocked(&room, &key.0);
                self.pending_commands.extend(commands);
            }
            UiEvent::RoomUnlockFailed { message } => self.on_room_unlock_failed(message),
            UiEvent::RoomLocked { room } => self.on_room_locked(room),
            UiEvent::InviteRejected => self.on_invite_rejected(),
            UiEvent::AudioDevicesUpdated {
                inputs, outputs, ..
            } => {
                self.audio.inputs = inputs;
                self.audio.outputs = outputs;
                self.audio.loaded = true;
            }
            UiEvent::Joined {
                room,
                local_identity,
                participants,
            } => self.on_joined(room, local_identity, participants),
            UiEvent::LocalAvatarChanged { version } => {
                self.avatar_notice = None;
                if self.avatar_version != version {
                    self.avatar_version = version;
                    self.pending_save = Some(self.settings_snapshot());
                }
            }
            UiEvent::AvatarFailed { message } => self.avatar_notice = Some(message),
            UiEvent::AvatarCached { .. } => {}
            UiEvent::CaptureTargetsUpdated(targets) => self.picker.set_targets(targets),
            UiEvent::CaptureThumbnail {
                id,
                width,
                height,
                rgba,
            } => self.picker.set_thumbnail(id, width, height, rgba),
            UiEvent::Failed { message } => {
                self.pending_move_notice = None;
                if matches!(self.screen, Screen::Connecting { .. } | Screen::InRoom(_)) {
                    self.lobby_error = Some(message);
                }
                self.leave_to_lobby();
            }
            UiEvent::MicLevel {
                level_db,
                gate_open,
            } => {
                if matches!(self.screen, Screen::InRoom(_)) && self.audio.noise.uses_own_capture() {
                    self.audio.mic_level = Some(MicLevelView {
                        level_db,
                        gate_open,
                    });
                }
            }
            UiEvent::Left => {
                self.pending_move_notice = None;
                self.leave_to_lobby();
            }
            UiEvent::AdminChanged { active } => {
                if active {
                    self.admin.active = true;
                    self.admin.logging_in = false;
                    self.admin.error = None;
                } else {
                    self.admin.reset();
                }
            }
            UiEvent::AdminRoomCreated(room) | UiEvent::AdminRoomUpdated(room) => {
                if !matches!(self.screen, Screen::Onboarding) {
                    upsert_admin_room(&mut self.rooms, room);
                }
            }
            UiEvent::AdminRoomDeleted { id } => {
                if !matches!(self.screen, Screen::Onboarding) {
                    self.rooms.retain(|r| r.name != id);
                    self.prune_admin_dialog_for_rooms();
                }
            }
            UiEvent::AdminInviteRotated => {
                let code = std::mem::take(&mut self.admin.new_invite_input);
                if !code.is_empty() {
                    self.active_password = code.clone();
                    if !self.form.invite_code.is_empty() {
                        self.form.invite_code = code.clone();
                    }
                    if let Some((server, _)) = self.saved_credential.take() {
                        self.saved_credential = Some((server.clone(), code.clone()));
                        self.pending_credential_ops.push(CredentialOp::Store {
                            server,
                            password: code,
                        });
                    }
                }
                self.admin
                    .set_notice("Código de convite atualizado.".to_owned());
            }
            UiEvent::AdminStoppedScreenShare => {
                self.set_chat_notice("Um admin parou sua transmissão".to_owned());
            }
            UiEvent::MovedByAdmin { to_room } => {
                let title = room_display_title(&self.rooms, &to_room).to_owned();
                let message = format!("Um admin moveu você para {title}");
                self.pending_move_notice = Some(message.clone());
                self.set_chat_notice(message);
                if matches!(
                    &self.admin.dialog,
                    Some(AdminDialog::KickParticipant { .. })
                ) {
                    self.admin.dialog = None;
                }
            }
            UiEvent::RemovedFromRoom { reason } => {
                self.pending_move_notice = None;
                if !matches!(self.screen, Screen::Onboarding | Screen::Lobby) {
                    self.leave_to_lobby();
                }
                if !matches!(self.screen, Screen::Onboarding) {
                    let message = match reason {
                        RemovalReason::Kicked => "Você foi removido da sala".to_owned(),
                        RemovalReason::RoomDeleted => "A sala foi apagada".to_owned(),
                    };
                    self.lobby_notice = Some(message);
                }
            }
            UiEvent::AdminFailed { message } => {
                self.admin.logging_in = false;
                self.admin.set_error(message.clone());
                if !self.audio.open {
                    match &mut self.screen {
                        Screen::InRoom(room) => room.chat.set_notice(message),
                        Screen::Lobby | Screen::Connecting { .. } => {
                            self.lobby_notice = Some(message);
                        }
                        Screen::Onboarding => {}
                    }
                }
            }
            other => {
                if let UiEvent::ParticipantLeft { ref identity } = other
                    && matches!(
                        &self.admin.dialog,
                        Some(AdminDialog::KickParticipant { identity: target, .. })
                            if target == identity
                    )
                {
                    self.admin.dialog = None;
                }
                let cue = if let Screen::InRoom(state) = &mut self.screen {
                    apply_room_event(state, other)
                } else {
                    None
                };
                if let Some(cmd) = cue.and_then(|cue| self.room_cue(cue)) {
                    self.pending_commands.push(cmd);
                }
            }
        }
    }
}
