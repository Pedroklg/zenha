//! UI commands and admin actions handled by the simulated session.

use std::time::Duration;

use zenha_core::{AdminAction, RemovalReason};
use zenha_protocol::AdminRoom;

use crate::ui_model::{UiCommand, UiEvent};

use super::fixtures::demo_targets;
use super::{DemoRoom, DemoSession};

impl DemoSession {
    pub(super) fn handle(&mut self, command: UiCommand) {
        match command {
            UiCommand::Join { room } => self.join(room),
            UiCommand::UnlockRoom { room, password } => self.unlock(&room, &password),
            UiCommand::Leave => {
                self.scheduled.clear();
                self.room = None;
                self.mic_meter = None;
                self.participants.clear();
                self.reset_share();
                self.schedule(Duration::ZERO, UiEvent::Left);
            }
            UiCommand::SetMuted(muted) => self.set_muted(muted),
            UiCommand::SetDeafened(_) | UiCommand::SetAvatar { .. } | UiCommand::ClearAvatar => {}
            UiCommand::SetDisplayName { name } => self.set_display_name(name),
            UiCommand::SendChat { text, save } => self.chat(text, save),
            UiCommand::LoadChatHistory { before } => self.load_chat_history(before),
            UiCommand::DeleteChat { id } => self.delete_chat(id),
            UiCommand::SendFile { path } => self.send_file(path),
            UiCommand::RefreshAudioDevices => self.publish_audio_devices(),
            UiCommand::SetInputDevice(id) => {
                self.input = id;
                self.publish_audio_devices();
            }
            UiCommand::SetNoiseGate(threshold) => {
                self.noise_gate = threshold;
            }
            UiCommand::SetNoiseSuppression(_)
            | UiCommand::SetEchoCancellation(_)
            | UiCommand::SetStreamVolume(_)
            | UiCommand::SetParticipantVolume { .. }
            | UiCommand::PlayCue(_) => {}
            UiCommand::SetOutputDevice(id) => {
                self.output = id;
                self.publish_audio_devices();
            }
            UiCommand::RefreshCaptureTargets => {
                self.scheduled
                    .retain(|s| !matches!(s.event, UiEvent::CaptureThumbnail { .. }));
                let targets = demo_targets();
                self.schedule(
                    Duration::ZERO,
                    UiEvent::CaptureTargetsUpdated(targets.clone()),
                );
                for (index, target) in targets.into_iter().enumerate() {
                    let width = zenha_core::CAPTURE_THUMBNAIL_MAX_SIDE;
                    let height = (width * 9 / 16).max(2);
                    let bgra =
                        zenha_core::TestPatternSource::render(width, height, index as u64 * 12);
                    let frame = zenha_core::BgraFrame {
                        width,
                        height,
                        stride: width as usize * 4,
                        data: &bgra,
                    };
                    if let Ok(vf) = zenha_core::bgra_to_video_frame(&frame, width) {
                        let delay = if self.delays.reply.is_zero() {
                            Duration::ZERO
                        } else {
                            Duration::from_millis((index as u64 + 1) * 90)
                        };
                        self.schedule(
                            delay,
                            UiEvent::CaptureThumbnail {
                                id: target.id,
                                width: vf.width as usize,
                                height: vf.height as usize,
                                rgba: vf.rgba,
                            },
                        );
                    }
                }
            }
            UiCommand::CancelCaptureTargets => self.cancel_capture_targets(),
            UiCommand::StartScreenShare { target_id, .. } => self.start_share(&target_id),
            UiCommand::StopScreenShare => {
                if self.share_preview.take().is_some() {
                    self.schedule(Duration::ZERO, UiEvent::ScreenShareStopped);
                }
            }
            UiCommand::WatchScreenShare { identity } => self.watch_share(identity),
            UiCommand::StopWatching => {
                if self.watch.take().is_some() {
                    self.schedule(Duration::ZERO, UiEvent::WatchingStopped);
                }
            }
            UiCommand::SetUiVisible(_) => {}
            UiCommand::RefreshRooms => {
                let rooms = self.rooms();
                self.schedule(Duration::ZERO, UiEvent::RoomsUpdated(rooms));
            }
            UiCommand::AdminLogin { password } => {
                if password.is_empty() {
                    self.schedule(
                        Duration::ZERO,
                        UiEvent::AdminFailed {
                            message: "Senha de administrador incorreta.".to_owned(),
                        },
                    );
                } else {
                    self.admin_active = true;
                    self.schedule(Duration::ZERO, UiEvent::AdminChanged { active: true });
                }
            }
            UiCommand::AdminLogout => {
                self.admin_active = false;
                self.schedule(Duration::ZERO, UiEvent::AdminChanged { active: false });
            }
            UiCommand::Admin(action) => self.handle_admin(action),
        }
    }

    pub(super) fn handle_admin(&mut self, action: AdminAction) {
        if !self.admin_active {
            self.schedule(
                Duration::ZERO,
                UiEvent::AdminFailed {
                    message: "Sessão de administrador expirada. Entre novamente.".to_owned(),
                },
            );
            return;
        }
        match action {
            AdminAction::CreateRoom {
                title,
                password,
                parent,
            } => {
                self.room_counter += 1;
                let id = format!("demo-sala-{}", self.room_counter);
                let position = self.rooms_list.len();
                let password = password.filter(|p| !p.is_empty());
                let parent = parent.filter(|p| !p.is_empty());
                let created = AdminRoom {
                    id: id.clone(),
                    title: title.clone(),
                    position,
                    locked: password.is_some(),
                    parent: parent.clone(),
                };
                self.rooms_list.push(DemoRoom {
                    id,
                    title,
                    idle_occupants: Vec::new(),
                    password,
                    parent,
                });
                self.schedule(Duration::ZERO, UiEvent::AdminRoomCreated(created));
                let rooms = self.rooms();
                self.schedule(Duration::ZERO, UiEvent::RoomsUpdated(rooms));
            }
            AdminAction::UpdateRoom {
                id,
                title,
                position,
                password,
                parent,
            } => {
                let Some(idx) = self.rooms_list.iter().position(|r| r.id == id) else {
                    self.schedule(
                        Duration::ZERO,
                        UiEvent::AdminFailed {
                            message: "Sala, participante ou mensagem não encontrado no servidor."
                                .to_owned(),
                        },
                    );
                    return;
                };
                let mut entry = self.rooms_list.remove(idx);
                if let Some(title) = title {
                    entry.title = title;
                }
                if let Some(password) = password {
                    entry.password = Some(password).filter(|p| !p.is_empty());
                    self.unlocked.retain(|owner| *owner != entry.id);
                }
                if let Some(parent) = parent {
                    entry.parent = Some(parent).filter(|p| !p.is_empty());
                }
                let final_pos = position.unwrap_or(idx).min(self.rooms_list.len());
                let updated = AdminRoom {
                    id: entry.id.clone(),
                    title: entry.title.clone(),
                    position: final_pos,
                    locked: entry.password.is_some(),
                    parent: entry.parent.clone(),
                };
                self.rooms_list.insert(final_pos, entry);
                self.schedule(Duration::ZERO, UiEvent::AdminRoomUpdated(updated));
                let rooms = self.rooms();
                self.schedule(Duration::ZERO, UiEvent::RoomsUpdated(rooms));
            }
            AdminAction::DeleteRoom { id } => {
                if self.rooms_list.len() <= 1 {
                    self.schedule(
                        Duration::ZERO,
                        UiEvent::AdminFailed {
                            message: "Não é possível apagar a última sala.".to_owned(),
                        },
                    );
                    return;
                }
                let before = self.rooms_list.len();
                self.rooms_list.retain(|r| r.id != id);
                for room in &mut self.rooms_list {
                    if room.parent.as_deref() == Some(id.as_str()) {
                        room.parent = None;
                    }
                }
                if self.rooms_list.len() == before {
                    self.schedule(
                        Duration::ZERO,
                        UiEvent::AdminFailed {
                            message: "Sala, participante ou mensagem não encontrado no servidor."
                                .to_owned(),
                        },
                    );
                    return;
                }
                if self.room.as_deref() == Some(id.as_str()) {
                    self.room = None;
                    self.mic_meter = None;
                    self.participants.clear();
                    self.share_preview = None;
                    self.watch = None;
                    self.ana_sharing = false;
                    self.schedule(Duration::ZERO, UiEvent::Left);
                    self.schedule(
                        Duration::ZERO,
                        UiEvent::RemovedFromRoom {
                            reason: RemovalReason::RoomDeleted,
                        },
                    );
                }
                self.schedule(Duration::ZERO, UiEvent::AdminRoomDeleted { id });
                let rooms = self.rooms();
                self.schedule(Duration::ZERO, UiEvent::RoomsUpdated(rooms));
            }
            AdminAction::MuteParticipant { identity, .. } => {
                if let Some(participant) = self
                    .participants
                    .iter_mut()
                    .find(|p| p.identity == identity)
                {
                    participant.muted = true;
                    let updated = participant.clone();
                    self.schedule(Duration::ZERO, UiEvent::ParticipantUpdated(updated));
                }
            }
            AdminAction::StopShareParticipant { identity, .. } => {
                if identity == "demo-ana" && self.ana_sharing {
                    self.ana_sharing = false;
                    if self.watch.take().is_some() {
                        self.schedule(Duration::ZERO, UiEvent::WatchingStopped);
                    }
                    self.schedule(
                        Duration::ZERO,
                        UiEvent::RemoteScreenShareStopped { identity },
                    );
                }
            }
            AdminAction::KickParticipant { identity, .. } => {
                let before = self.participants.len();
                self.participants.retain(|p| p.identity != identity);
                if self.participants.len() != before {
                    if identity == "demo-ana" && self.ana_sharing {
                        self.ana_sharing = false;
                        if self.watch.take().is_some() {
                            self.schedule(Duration::ZERO, UiEvent::WatchingStopped);
                        }
                        self.schedule(
                            Duration::ZERO,
                            UiEvent::RemoteScreenShareStopped {
                                identity: identity.clone(),
                            },
                        );
                    }
                    self.schedule(Duration::ZERO, UiEvent::ParticipantLeft { identity });
                    let rooms = self.rooms();
                    self.schedule(Duration::ZERO, UiEvent::RoomsUpdated(rooms));
                }
            }
            AdminAction::MoveParticipant {
                identity, to_room, ..
            } => {
                if let Some(pos) = self
                    .participants
                    .iter()
                    .position(|p| p.identity == identity)
                {
                    let removed = self.participants.remove(pos);
                    if let Some(dest) = self.rooms_list.iter_mut().find(|r| r.id == to_room) {
                        dest.idle_occupants.push(removed.name);
                    }
                    self.schedule(Duration::ZERO, UiEvent::ParticipantLeft { identity });
                    let rooms = self.rooms();
                    self.schedule(Duration::ZERO, UiEvent::RoomsUpdated(rooms));
                }
            }
            AdminAction::DeleteChat { id, .. } => {
                self.schedule(
                    Duration::ZERO,
                    UiEvent::ChatDeleted {
                        id,
                        sender: String::new(),
                    },
                );
            }
            AdminAction::RotateInvite { .. } => {
                self.schedule(Duration::ZERO, UiEvent::AdminInviteRotated);
            }
            AdminAction::ListWebhooks => self.list_webhooks(),
            AdminAction::CreateWebhook { room, name } => self.create_webhook(room, name),
            AdminAction::DeleteWebhook { id } => self.delete_webhook(id),
        }
    }
}
