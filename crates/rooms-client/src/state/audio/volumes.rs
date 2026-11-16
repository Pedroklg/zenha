//! Controle de volume da transmissão e volumes individuais por participante.

use std::collections::BTreeMap;

use crate::ui_model::UiCommand;

pub const FULL_VOLUME: u8 = 100;
pub const DEFAULT_PARTICIPANT_VOLUME: u16 = 100;
pub const MAX_PARTICIPANT_VOLUME: u16 = 200;

pub fn clamp_participant_volume(percent: u16) -> u16 {
    percent.min(MAX_PARTICIPANT_VOLUME)
}

pub fn participant_volume_for(volumes: &BTreeMap<String, u16>, identity: &str) -> u16 {
    volumes
        .get(identity)
        .copied()
        .map(clamp_participant_volume)
        .unwrap_or(DEFAULT_PARTICIPANT_VOLUME)
}

/// Applies `percent` to `volumes`, omitting `100%`, and returns the new value when it changed.
pub fn update_participant_volume(
    volumes: &mut BTreeMap<String, u16>,
    identity: String,
    percent: u16,
) -> Option<u16> {
    if identity.trim().is_empty() {
        return None;
    }
    let clamped = clamp_participant_volume(percent);
    if clamped == DEFAULT_PARTICIPANT_VOLUME {
        volumes
            .remove(&identity)
            .map(|_| DEFAULT_PARTICIPANT_VOLUME)
    } else if volumes.get(&identity).copied() == Some(clamped) {
        None
    } else {
        volumes.insert(identity, clamped);
        Some(clamped)
    }
}

pub fn normalize_participant_volumes(raw: &BTreeMap<String, u16>) -> BTreeMap<String, u16> {
    let mut normalized = BTreeMap::new();
    for (identity, &percent) in raw {
        update_participant_volume(&mut normalized, identity.clone(), percent);
    }
    normalized
}

pub fn saved_volume_commands(volumes: &BTreeMap<String, u16>) -> Vec<UiCommand> {
    normalize_participant_volumes(volumes)
        .into_iter()
        .map(|(identity, percent)| UiCommand::SetParticipantVolume { identity, percent })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::valid_settings;
    use crate::state::{AppState, Screen, UserAction};

    #[test]
    fn stream_volume_applies_while_moving_and_saves_when_asked() {
        let mut state = AppState::new(&valid_settings(), None);
        assert_eq!(state.audio.stream_volume, FULL_VOLUME);
        assert_eq!(
            state.handle_action(UserAction::SetStreamVolume(40)),
            vec![UiCommand::SetStreamVolume(40)]
        );
        assert!(state.take_pending_save().is_none());
        assert!(
            state
                .handle_action(UserAction::SetStreamVolume(40))
                .is_empty()
        );
        state.handle_action(UserAction::SaveAudioSettings);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(saved.stream_volume, Some(40));

        let mut restored = AppState::new(&saved, Some("segredo".to_owned()));
        assert_eq!(restored.screen, Screen::Lobby);
        let commands = restored.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(commands.contains(&UiCommand::SetStreamVolume(40)));
    }

    #[test]
    fn participant_volume_helpers_clamp_normalize_and_emit_commands() {
        assert_eq!(clamp_participant_volume(0), 0);
        assert_eq!(clamp_participant_volume(140), 140);
        assert_eq!(clamp_participant_volume(260), 200);

        let raw = BTreeMap::from([
            ("alice".to_owned(), 70),
            ("bob".to_owned(), 100),
            ("carol".to_owned(), 250),
            ("   ".to_owned(), 40),
        ]);
        let normalized = normalize_participant_volumes(&raw);
        assert_eq!(
            normalized,
            BTreeMap::from([("alice".to_owned(), 70), ("carol".to_owned(), 200)])
        );
        assert_eq!(participant_volume_for(&normalized, "alice"), 70);
        assert_eq!(participant_volume_for(&normalized, "bob"), 100);
        assert_eq!(participant_volume_for(&normalized, "carol"), 200);
        assert_eq!(
            saved_volume_commands(&raw),
            vec![
                UiCommand::SetParticipantVolume {
                    identity: "alice".to_owned(),
                    percent: 70,
                },
                UiCommand::SetParticipantVolume {
                    identity: "carol".to_owned(),
                    percent: 200,
                },
            ]
        );
    }

    #[test]
    fn participant_volume_updates_live_without_saving_removes_100_and_sends_on_startup_and_join() {
        let mut state = AppState::new(&valid_settings(), Some("segredo".to_owned()));
        assert!(state.take_pending_commands().is_empty());

        // Setting 100 when absent is a no-op.
        assert!(
            state
                .handle_action(UserAction::SetParticipantVolume {
                    identity: "alice".to_owned(),
                    percent: 100,
                })
                .is_empty()
        );

        // Adjusting clamps above 200 and sends to core without triggering a settings save.
        assert_eq!(
            state.handle_action(UserAction::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 250,
            }),
            vec![UiCommand::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 200,
            }]
        );
        assert!(state.take_pending_save().is_none());
        assert_eq!(
            state.handle_action(UserAction::SetParticipantVolume {
                identity: "bob".to_owned(),
                percent: 65,
            }),
            vec![UiCommand::SetParticipantVolume {
                identity: "bob".to_owned(),
                percent: 65,
            }]
        );
        // Repeating the same value does not spam commands.
        assert!(
            state
                .handle_action(UserAction::SetParticipantVolume {
                    identity: "bob".to_owned(),
                    percent: 65,
                })
                .is_empty()
        );

        // Resetting bob to 100 sends 100 to core and removes the entry before saving.
        assert_eq!(
            state.handle_action(UserAction::SetParticipantVolume {
                identity: "bob".to_owned(),
                percent: 100,
            }),
            vec![UiCommand::SetParticipantVolume {
                identity: "bob".to_owned(),
                percent: 100,
            }]
        );
        assert!(state.take_pending_save().is_none());

        state.handle_action(UserAction::SaveAudioSettings);
        let saved = state.take_pending_save().unwrap();
        assert_eq!(
            saved.participant_volumes,
            BTreeMap::from([("alice".to_owned(), 200)])
        );

        // Next app startup queues SetParticipantVolume immediately and also sends on first join.
        let mut restarted =
            AppState::new_with_build_default(&saved, Some("segredo".to_owned()), None);
        assert_eq!(restarted.screen, Screen::Lobby);
        assert_eq!(
            restarted.take_pending_commands(),
            vec![UiCommand::SetParticipantVolume {
                identity: "alice".to_owned(),
                percent: 200,
            }]
        );
        let join_cmds = restarted.handle_action(UserAction::JoinRoom("geral".to_owned()));
        assert!(join_cmds.contains(&UiCommand::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 200,
        }));
    }
}
