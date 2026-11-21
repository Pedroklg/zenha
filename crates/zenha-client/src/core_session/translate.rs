//! Translation of core events into UI events.

use std::cell::Cell;

use zenha_core::{
    AudioDevice, CaptureKind, CaptureTarget, Event, Participant, SessionError, TransferState,
    is_image_mime,
};

use crate::ui_model::{
    AudioDeviceView, CaptureTargetView, ChatMessageView, FileTransferView, ParticipantView,
    TargetKindView, TransferStateView, UiEvent,
};

use super::errors::{describe, is_admin_error, is_share_error};
use super::feed::feed_handle;

fn target_view(target: CaptureTarget) -> CaptureTargetView {
    CaptureTargetView {
        id: target.id,
        name: target.name,
        kind: match target.kind {
            CaptureKind::Screen => TargetKindView::Screen,
            CaptureKind::Window => TargetKindView::Window,
        },
    }
}

fn device_view(device: AudioDevice) -> AudioDeviceView {
    AudioDeviceView {
        id: device.id,
        name: device.name,
    }
}

fn view(participant: Participant) -> ParticipantView {
    ParticipantView {
        identity: participant.identity,
        name: participant.name,
        muted: participant.muted,
        deafened: participant.deafened,
        speaking: participant.speaking,
        avatar_version: participant.avatar_version,
    }
}

/// Converts core events into UI events, keeping what the
/// interface expects to receive whole (full participant, current room).
#[derive(Default)]
pub(super) struct Translator {
    pub(super) room: Option<String>,
    local_identity: String,
    pub(super) participants: Vec<ParticipantView>,
    pub(super) switch_pending: Cell<bool>,
}

impl Translator {
    fn chat_message_view(&self, message: zenha_protocol::ChatMessage) -> ChatMessageView {
        let sender = self
            .participants
            .iter()
            .find(|p| p.identity == message.sender)
            .map_or_else(|| message.sender.clone(), |p| p.name.clone());
        ChatMessageView {
            id: message.id,
            sender_id: message.sender,
            sender,
            sent_at_unix_ms: message.sent_at_unix_ms,
            text: message.text,
            saved: message.saved,
        }
    }

    pub(super) fn translate(&mut self, event: Event) -> Vec<UiEvent> {
        match event {
            Event::Connecting => vec![UiEvent::Connecting],
            Event::Joined {
                room,
                local_identity,
                participants,
            } => {
                self.room = Some(room.clone());
                self.local_identity = local_identity.clone();
                self.participants = participants.into_iter().map(view).collect();
                vec![UiEvent::Joined {
                    room,
                    local_identity,
                    participants: self.participants.clone(),
                }]
            }
            Event::ParticipantList(participants) => {
                self.participants = participants.into_iter().map(view).collect();
                match &self.room {
                    Some(room) => vec![UiEvent::Joined {
                        room: room.clone(),
                        local_identity: self.local_identity.clone(),
                        participants: self.participants.clone(),
                    }],
                    None => Vec::new(),
                }
            }
            Event::Reconnecting => vec![UiEvent::Reconnecting],
            Event::Reconnected => vec![UiEvent::Reconnected],
            Event::Left => {
                self.participants.clear();
                // On a room switch the core reports Left before the new Joined;
                // the interface only wants Left when the user really left.
                if self.switch_pending.take() {
                    Vec::new()
                } else {
                    self.room = None;
                    self.local_identity.clear();
                    vec![UiEvent::Left]
                }
            }
            Event::Failed(error) => {
                self.switch_pending.set(false);
                self.room = None;
                self.local_identity.clear();
                self.participants.clear();
                if matches!(error, SessionError::InviteRejected) {
                    vec![UiEvent::InviteRejected]
                } else if let SessionError::RoomLocked(room) = error {
                    vec![UiEvent::RoomLocked { room }]
                } else {
                    vec![UiEvent::Failed {
                        message: describe(&error),
                    }]
                }
            }
            Event::ParticipantJoined(participant) => {
                let participant = view(participant);
                self.participants
                    .retain(|p| p.identity != participant.identity);
                self.participants.push(participant.clone());
                vec![UiEvent::ParticipantJoined(participant)]
            }
            Event::ParticipantLeft { identity } => {
                self.participants.retain(|p| p.identity != identity);
                vec![UiEvent::ParticipantLeft { identity }]
            }
            Event::ParticipantMuteChanged { identity, muted } => {
                self.update(&identity, |p| p.muted = muted)
            }
            Event::ParticipantDeafenChanged { identity, deafened } => {
                self.update(&identity, |p| p.deafened = deafened)
            }
            Event::ParticipantSpeakingChanged { identity, speaking } => {
                self.update(&identity, |p| p.speaking = speaking)
            }
            Event::ParticipantNameChanged { identity, name } => {
                self.update(&identity, |p| p.name = name)
            }
            Event::ParticipantAvatarChanged { identity, version } => {
                self.update(&identity, |p| p.avatar_version = version)
            }
            Event::LocalAvatarChanged { version } => {
                vec![UiEvent::LocalAvatarChanged { version }]
            }
            Event::AvatarCached {
                identity,
                version,
                path,
            } => vec![UiEvent::AvatarCached {
                identity,
                version,
                path,
            }],
            Event::ChatReceived(message) => {
                vec![UiEvent::ChatReceived(self.chat_message_view(message))]
            }
            Event::FileTransfer(transfer) => {
                let sender = self
                    .participants
                    .iter()
                    .find(|p| p.identity == transfer.sender)
                    .map_or(transfer.sender, |p| p.name.clone());
                vec![UiEvent::FileTransfer(FileTransferView {
                    id: transfer.id,
                    sender,
                    is_image: is_image_mime(&transfer.mime),
                    name: transfer.name,
                    size: transfer.size,
                    outgoing: transfer.outgoing,
                    sent_at_unix_ms: transfer.sent_at_unix_ms,
                    state: match transfer.state {
                        TransferState::InProgress { bytes } => {
                            TransferStateView::InProgress { bytes }
                        }
                        TransferState::Done { path } => TransferStateView::Done { path },
                        TransferState::Failed(message) => TransferStateView::Failed(message),
                    },
                })]
            }
            Event::RoomsUpdated(rooms) => vec![UiEvent::RoomsUpdated(rooms)],
            Event::RoomUnlocked { room, key } => vec![UiEvent::RoomUnlocked { room, key }],
            Event::AudioDevicesUpdated(devices) => vec![UiEvent::AudioDevicesUpdated {
                inputs: devices.inputs.into_iter().map(device_view).collect(),
                outputs: devices.outputs.into_iter().map(device_view).collect(),
                selected_input: devices.selected_input,
                selected_output: devices.selected_output,
            }],
            Event::CaptureTargetsUpdated(targets) => vec![UiEvent::CaptureTargetsUpdated(
                targets.into_iter().map(target_view).collect(),
            )],
            Event::CaptureThumbnail {
                id,
                width,
                height,
                rgba,
            } => vec![UiEvent::CaptureThumbnail {
                id,
                width: width as usize,
                height: height as usize,
                rgba,
            }],
            Event::MicLevel {
                level_db,
                gate_open,
            } => vec![UiEvent::MicLevel {
                level_db,
                gate_open,
            }],
            Event::ScreenShareStarted { preview } => vec![UiEvent::ScreenShareStarted {
                preview: feed_handle(preview),
            }],
            Event::ScreenShareStopped => vec![UiEvent::ScreenShareStopped],
            Event::RemoteScreenShareStarted { identity } => {
                vec![UiEvent::RemoteScreenShareStarted { identity }]
            }
            Event::RemoteScreenShareStopped { identity } => {
                vec![UiEvent::RemoteScreenShareStopped { identity }]
            }
            Event::WatchingStarted { identity, frames } => vec![UiEvent::WatchingStarted {
                identity,
                feed: feed_handle(frames),
            }],
            Event::WatchingStopped => vec![UiEvent::WatchingStopped],
            Event::ChatHistory {
                messages,
                has_more,
                before,
                webhook_posts,
            } => {
                let messages = messages
                    .into_iter()
                    .map(|item| ChatMessageView {
                        id: item.message.id,
                        sender_id: item.sender_id,
                        sender: item.message.sender,
                        sent_at_unix_ms: item.message.sent_at_unix_ms,
                        text: item.message.text,
                        saved: item.message.saved,
                    })
                    .collect();
                vec![UiEvent::ChatHistory {
                    messages,
                    has_more,
                    before,
                    webhook_posts,
                }]
            }
            Event::ChatDeleted { id, sender } => vec![UiEvent::ChatDeleted { id, sender }],
            Event::AdminChanged { active } => vec![UiEvent::AdminChanged { active }],
            Event::AdminRoomCreated(room) => vec![UiEvent::AdminRoomCreated(room)],
            Event::AdminRoomUpdated(room) => vec![UiEvent::AdminRoomUpdated(room)],
            Event::AdminRoomDeleted { id } => vec![UiEvent::AdminRoomDeleted { id }],
            Event::AdminInviteRotated => vec![UiEvent::AdminInviteRotated],
            Event::AdminWebhooks(webhooks) => vec![UiEvent::AdminWebhooks(webhooks)],
            Event::AdminWebhookCreated(created) => vec![UiEvent::AdminWebhookCreated(created)],
            Event::AdminWebhookDeleted { id } => vec![UiEvent::AdminWebhookDeleted { id }],
            Event::WebhookPosted(post) => vec![UiEvent::WebhookPosted(post)],
            Event::AdminStoppedScreenShare => vec![UiEvent::AdminStoppedScreenShare],
            Event::MovedByAdmin { to_room } => {
                if self.room.is_some() {
                    self.switch_pending.set(true);
                }
                vec![UiEvent::MovedByAdmin { to_room }]
            }
            Event::RemovedFromRoom { reason } => {
                self.switch_pending.set(false);
                self.room = None;
                self.local_identity.clear();
                self.participants.clear();
                vec![UiEvent::RemovedFromRoom { reason }]
            }
            Event::CommandFailed(error) => {
                tracing::warn!(%error, "command failed");
                match &error {
                    SessionError::InviteRejected => vec![UiEvent::InviteRejected],
                    SessionError::RoomPasswordRejected | SessionError::RoomUnlockRateLimited => {
                        vec![UiEvent::RoomUnlockFailed {
                            message: describe(&error),
                        }]
                    }
                    SessionError::Avatar(_) => vec![UiEvent::AvatarFailed {
                        message: describe(&error),
                    }],
                    SessionError::RateLimited
                    | SessionError::Unauthorized
                    | SessionError::Chat(_) => vec![UiEvent::ChatFailed {
                        message: describe(&error),
                    }],
                    SessionError::TokenServer(_) => vec![UiEvent::RoomsFailed {
                        message: describe(&error),
                    }],
                    _ if is_share_error(&error) => vec![UiEvent::ShareFailed {
                        message: describe(&error),
                    }],
                    _ if is_admin_error(&error) => vec![UiEvent::AdminFailed {
                        message: describe(&error),
                    }],
                    _ => Vec::new(),
                }
            }
        }
    }

    fn update(
        &mut self,
        identity: &str,
        change: impl FnOnce(&mut ParticipantView),
    ) -> Vec<UiEvent> {
        match self
            .participants
            .iter_mut()
            .find(|p| p.identity == identity)
        {
            Some(participant) => {
                change(participant);
                vec![UiEvent::ParticipantUpdated(participant.clone())]
            }
            None => Vec::new(),
        }
    }
}
