//! State of the available-update notice and the footer state machine.

use std::path::PathBuf;

use super::{AppState, Screen};
use crate::ui_model::UiCommand;
use crate::updater::{AvailableUpdate, InstallerAsset, UpdateDownloadEvent, should_show_update};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdatePhase {
    #[default]
    Available,
    Confirming,
    Downloading {
        downloaded: u64,
        total: u64,
    },
    Ready(PathBuf),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateBanner {
    pub version: String,
    pub url: String,
    pub can_install: bool,
    pub phase: UpdatePhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEffect {
    StartDownload {
        id: u64,
        version: String,
        asset: InstallerAsset,
    },
    CancelDownload {
        id: u64,
    },
    RunInstaller {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct UpdateState {
    pub(super) available: Option<AvailableUpdate>,
    pub(super) dismissed_version: Option<String>,
    pub(super) can_auto_update: bool,
    pub(super) phase: UpdatePhase,
    confirmed_in_room: bool,
    download_seq: u64,
    active_download_id: Option<u64>,
}

impl UpdateState {
    pub(super) fn with_dismissed(dismissed_version: Option<String>) -> Self {
        Self {
            dismissed_version,
            ..Self::default()
        }
    }

    pub(super) fn on_left_room(&mut self) {
        self.confirmed_in_room = false;
        if matches!(self.phase, UpdatePhase::Confirming) {
            self.phase = UpdatePhase::Available;
        }
    }
}

impl AppState {
    pub fn set_can_auto_update(&mut self, can_auto_update: bool) {
        self.update.can_auto_update = can_auto_update;
    }

    pub fn set_available_update(&mut self, update: AvailableUpdate) {
        let same_version = self
            .update
            .available
            .as_ref()
            .is_some_and(|current| current.version == update.version);
        if !same_version {
            if let Some(id) = self.update.active_download_id.take() {
                self.pending_update_effects
                    .push(UpdateEffect::CancelDownload { id });
            }
            self.update.phase = UpdatePhase::Available;
            self.update.confirmed_in_room = false;
        }
        self.update.available = Some(update);
    }

    pub fn visible_update(&self) -> Option<&AvailableUpdate> {
        self.update
            .available
            .as_ref()
            .filter(|update| should_show_update(update, self.update.dismissed_version.as_deref()))
    }

    pub fn visible_update_banner(&self) -> Option<UpdateBanner> {
        self.visible_update().map(|update| UpdateBanner {
            version: update.version.clone(),
            url: update.url.clone(),
            can_install: self.update.can_auto_update && update.installer.is_some(),
            phase: self.update.phase.clone(),
        })
    }

    pub fn is_update_confirming(&self) -> bool {
        self.visible_update().is_some() && matches!(self.update.phase, UpdatePhase::Confirming)
    }

    pub fn needs_update_confirmation(&self) -> bool {
        matches!(self.screen, Screen::Connecting { .. } | Screen::InRoom(_))
            || self.is_sharing_local()
    }

    pub fn take_update_effects(&mut self) -> Vec<UpdateEffect> {
        std::mem::take(&mut self.pending_update_effects)
    }

    pub fn apply_update_event(&mut self, event: UpdateDownloadEvent) {
        match event {
            UpdateDownloadEvent::Progress {
                id,
                downloaded,
                total,
            } => {
                if self.update.active_download_id == Some(id)
                    && matches!(self.update.phase, UpdatePhase::Downloading { .. })
                {
                    self.update.phase = UpdatePhase::Downloading {
                        downloaded: downloaded.min(total),
                        total,
                    };
                }
            }
            UpdateDownloadEvent::Finished { id, result } => {
                if self.update.active_download_id == Some(id)
                    && matches!(self.update.phase, UpdatePhase::Downloading { .. })
                {
                    self.update.active_download_id = None;
                    match result {
                        Ok(path) => {
                            self.update.phase = UpdatePhase::Ready(path.clone());
                            self.pending_update_effects
                                .push(UpdateEffect::RunInstaller { path });
                        }
                        Err(message) => {
                            self.update.phase = UpdatePhase::Failed(message);
                        }
                    }
                }
            }
        }
    }

    pub fn on_installer_launch_failed(&mut self, message: String) {
        self.update.phase = UpdatePhase::Failed(message);
    }

    pub fn leave_for_update(&mut self) -> Vec<UiCommand> {
        match &self.screen {
            Screen::InRoom(_) => self.leave(),
            Screen::Connecting { .. } => self.cancel_connect(),
            Screen::Lobby | Screen::Onboarding => Vec::new(),
        }
    }

    pub(super) fn start_update(&mut self) {
        let Some(update) = self.visible_update().cloned() else {
            return;
        };
        if !self.update.can_auto_update || update.installer.is_none() {
            return;
        }
        if !matches!(self.update.phase, UpdatePhase::Available) {
            return;
        }
        if self.needs_update_confirmation() {
            self.update.phase = UpdatePhase::Confirming;
        } else {
            self.begin_update_download(update);
        }
    }

    pub(super) fn confirm_update(&mut self) {
        if !matches!(self.update.phase, UpdatePhase::Confirming) {
            return;
        }
        let Some(update) = self.visible_update().cloned() else {
            self.update.phase = UpdatePhase::Available;
            return;
        };
        if !self.update.can_auto_update || update.installer.is_none() {
            self.update.phase = UpdatePhase::Available;
            return;
        }
        self.update.confirmed_in_room = true;
        self.begin_update_download(update);
    }

    pub(super) fn cancel_update_confirm(&mut self) {
        if matches!(self.update.phase, UpdatePhase::Confirming) {
            self.update.phase = UpdatePhase::Available;
            self.update.confirmed_in_room = false;
        }
    }

    pub(super) fn cancel_update_download(&mut self) {
        if matches!(self.update.phase, UpdatePhase::Downloading { .. }) {
            self.update.phase = UpdatePhase::Available;
            self.update.confirmed_in_room = false;
            if let Some(id) = self.update.active_download_id.take() {
                self.pending_update_effects
                    .push(UpdateEffect::CancelDownload { id });
            }
        }
    }

    pub(super) fn retry_update(&mut self) {
        if !matches!(self.update.phase, UpdatePhase::Failed(_)) {
            return;
        }
        let Some(update) = self.visible_update().cloned() else {
            return;
        };
        if !self.update.can_auto_update || update.installer.is_none() {
            return;
        }
        if self.needs_update_confirmation() && !self.update.confirmed_in_room {
            self.update.phase = UpdatePhase::Confirming;
        } else {
            self.begin_update_download(update);
        }
    }

    pub(super) fn dismiss_update(&mut self) {
        if let Some(id) = self.update.active_download_id.take() {
            self.pending_update_effects
                .push(UpdateEffect::CancelDownload { id });
        }
        self.update.phase = UpdatePhase::Available;
        self.update.confirmed_in_room = false;
        if let Some(update) = &self.update.available {
            self.update.dismissed_version = Some(update.version.clone());
            self.pending_save = Some(self.settings_snapshot());
        }
    }

    fn begin_update_download(&mut self, update: AvailableUpdate) {
        let Some(asset) = update.installer else {
            return;
        };
        self.update.download_seq = self.update.download_seq.wrapping_add(1);
        let id = self.update.download_seq;
        let total = asset.size;
        self.update.active_download_id = Some(id);
        self.update.phase = UpdatePhase::Downloading {
            downloaded: 0,
            total,
        };
        self.pending_update_effects
            .push(UpdateEffect::StartDownload {
                id,
                version: update.version,
                asset,
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UserAction;
    use crate::state::test_support::{in_room, state_with_code, valid_settings};
    use crate::updater::InstallerAsset;

    fn sample_installer() -> InstallerAsset {
        InstallerAsset {
            url: "https://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe".to_owned(),
            size: 1_000,
            sha256: [7u8; 32],
        }
    }

    fn installable_update(version: &str) -> AvailableUpdate {
        AvailableUpdate {
            version: version.to_owned(),
            url: format!("https://github.com/Pedroklg/zenha/releases/tag/v{version}"),
            installer: Some(sample_installer()),
        }
    }

    #[test]
    fn available_update_dismisses_and_reappears_for_newer_version() {
        let mut state =
            AppState::new_with_build_default(&valid_settings(), Some("segredo".to_owned()), None);
        assert_eq!(state.visible_update(), None);

        let v2 = AvailableUpdate {
            version: "0.2.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0".to_owned(),
            installer: None,
        };
        state.set_available_update(v2.clone());
        assert_eq!(state.visible_update(), Some(&v2));

        state.handle_action(UserAction::DismissUpdate);
        assert_eq!(state.visible_update(), None);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.dismissed_update_version.as_deref(), Some("0.2.0"));

        let mut reloaded =
            AppState::new_with_build_default(&saved, Some("segredo".to_owned()), None);
        reloaded.set_available_update(v2);
        assert_eq!(reloaded.visible_update(), None);

        let v3 = AvailableUpdate {
            version: "0.3.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.3.0".to_owned(),
            installer: None,
        };
        reloaded.set_available_update(v3.clone());
        assert_eq!(reloaded.visible_update(), Some(&v3));
    }

    #[test]
    fn portable_or_link_only_release_never_starts_auto_update() {
        let mut state =
            AppState::new_with_build_default(&valid_settings(), Some("segredo".to_owned()), None);
        state.set_can_auto_update(false);
        state.set_available_update(installable_update("0.2.0"));

        let banner = state.visible_update_banner().unwrap();
        assert!(!banner.can_install);
        assert_eq!(banner.phase, UpdatePhase::Available);

        state.handle_action(UserAction::StartUpdate);
        assert!(state.take_update_effects().is_empty());
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Available
        );

        state.set_can_auto_update(true);
        state.set_available_update(AvailableUpdate {
            version: "0.3.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.3.0".to_owned(),
            installer: None,
        });
        assert!(!state.visible_update_banner().unwrap().can_install);
        state.handle_action(UserAction::StartUpdate);
        assert!(state.take_update_effects().is_empty());
    }

    #[test]
    fn lobby_update_transitions_through_download_cancel_error_retry_and_ready() {
        let mut state =
            AppState::new_with_build_default(&valid_settings(), Some("segredo".to_owned()), None);
        state.set_can_auto_update(true);
        state.set_available_update(installable_update("0.2.0"));

        state.handle_action(UserAction::StartUpdate);
        let effects = state.take_update_effects();
        assert_eq!(
            effects,
            vec![UpdateEffect::StartDownload {
                id: 1,
                version: "0.2.0".to_owned(),
                asset: sample_installer(),
            }]
        );
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Downloading {
                downloaded: 0,
                total: 1_000,
            }
        );

        state.apply_update_event(UpdateDownloadEvent::Progress {
            id: 1,
            downloaded: 400,
            total: 1_000,
        });
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Downloading {
                downloaded: 400,
                total: 1_000,
            }
        );

        // Canceling emits CancelDownload and ignores stale events from id 1.
        state.handle_action(UserAction::CancelUpdateDownload);
        assert_eq!(
            state.take_update_effects(),
            vec![UpdateEffect::CancelDownload { id: 1 }]
        );
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Available
        );
        state.apply_update_event(UpdateDownloadEvent::Finished {
            id: 1,
            result: Ok(PathBuf::from("stale.exe")),
        });
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Available
        );
        assert!(state.take_update_effects().is_empty());

        // Start again -> id 2 fails -> Failed -> Retry -> id 3 finishes -> Ready.
        state.handle_action(UserAction::StartUpdate);
        assert!(matches!(
            &state.take_update_effects()[..],
            [UpdateEffect::StartDownload { id: 2, .. }]
        ));
        state.apply_update_event(UpdateDownloadEvent::Finished {
            id: 2,
            result: Err("Hash do arquivo não confere.".to_owned()),
        });
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Failed("Hash do arquivo não confere.".to_owned())
        );

        state.handle_action(UserAction::RetryUpdate);
        assert!(matches!(
            &state.take_update_effects()[..],
            [UpdateEffect::StartDownload { id: 3, .. }]
        ));
        let exe_path = PathBuf::from(r"C:\Temp\zenha-update\Zenha-0.2.0-setup.exe");
        state.apply_update_event(UpdateDownloadEvent::Finished {
            id: 3,
            result: Ok(exe_path.clone()),
        });
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Ready(exe_path.clone())
        );
        assert_eq!(
            state.take_update_effects(),
            vec![UpdateEffect::RunInstaller { path: exe_path }]
        );
    }

    #[test]
    fn room_update_requires_confirmation_and_resets_if_room_exited() {
        let mut state = state_with_code();
        state.set_can_auto_update(true);
        state.set_available_update(installable_update("0.2.0"));
        in_room(&mut state);
        state.take_pending_commands();

        assert!(state.needs_update_confirmation());
        state.handle_action(UserAction::StartUpdate);
        assert!(state.is_update_confirming());
        assert!(!state.can_paste_chat_image());
        assert!(state.take_update_effects().is_empty());

        // Cancel confirmation keeps user in room and returns banner to Available.
        state.handle_action(UserAction::CancelUpdateConfirm);
        assert!(!state.is_update_confirming());
        assert!(state.can_paste_chat_image());
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Available
        );

        // Confirm starts download; failure inside room retries directly without re-prompting.
        state.handle_action(UserAction::StartUpdate);
        assert!(state.is_update_confirming());
        state.handle_action(UserAction::ConfirmUpdate);
        assert!(!state.is_update_confirming());
        assert!(matches!(
            &state.take_update_effects()[..],
            [UpdateEffect::StartDownload { id: 1, .. }]
        ));

        state.apply_update_event(UpdateDownloadEvent::Finished {
            id: 1,
            result: Err("Falha de rede.".to_owned()),
        });
        state.handle_action(UserAction::RetryUpdate);
        assert!(!state.is_update_confirming());
        assert!(matches!(
            &state.take_update_effects()[..],
            [UpdateEffect::StartDownload { id: 2, .. }]
        ));

        // Ready state leaves room cleanly via leave_for_update.
        let exe_path = PathBuf::from(r"C:\Temp\zenha-update\Zenha-0.2.0-setup.exe");
        state.apply_update_event(UpdateDownloadEvent::Finished {
            id: 2,
            result: Ok(exe_path),
        });
        assert_eq!(
            state.take_update_effects(),
            vec![UpdateEffect::RunInstaller {
                path: PathBuf::from(r"C:\Temp\zenha-update\Zenha-0.2.0-setup.exe"),
            }]
        );
        let leave_for_update = state.leave_for_update();
        assert!(leave_for_update.contains(&UiCommand::Leave));
        assert!(matches!(state.screen, Screen::Lobby));
        assert!(state.leave_for_update().is_empty());

        // Leaving room while Confirming closes the modal automatically.
        in_room(&mut state);
        state.set_available_update(installable_update("0.3.0"));
        state.handle_action(UserAction::StartUpdate);
        assert!(state.is_update_confirming());
        let leave_cmds = state.handle_action(UserAction::Leave);
        assert!(leave_cmds.contains(&UiCommand::Leave));
        assert!(!state.is_update_confirming());
        assert_eq!(
            state.visible_update_banner().unwrap().phase,
            UpdatePhase::Available
        );
    }
}
