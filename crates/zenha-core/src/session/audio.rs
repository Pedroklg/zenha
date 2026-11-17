//! Audio control, noise suppression, per-participant volume and devices.

use crate::api::{Event, NoiseSuppression};

use super::Session;
use super::connection::Phase;

const NOISE_GATE_MIN_DB: i8 = -70;
const NOISE_GATE_MAX_DB: i8 = -20;

impl Session {
    pub(super) async fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if matches!(self.phase, Phase::Connected { .. })
            && let Err(err) = self.backend.set_microphone_muted(muted).await
        {
            self.emit(Event::CommandFailed(err));
        }
    }

    pub(super) async fn set_deafened(&mut self, deafened: bool) {
        self.deafened = deafened;
        if matches!(self.phase, Phase::Connected { .. })
            && let Err(err) = self.backend.set_output_muted(deafened).await
        {
            self.emit(Event::CommandFailed(err));
        }
    }

    pub(super) async fn publish_audio_devices(&mut self) {
        match self.backend.audio_devices().await {
            Ok(devices) => self.emit(Event::AudioDevicesUpdated(devices)),
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) async fn set_input_device(&mut self, id: Option<String>) {
        match self.backend.set_input_device(id).await {
            Ok(()) => self.publish_audio_devices().await,
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) async fn set_output_device(&mut self, id: Option<String>) {
        match self.backend.set_output_device(id).await {
            Ok(()) => self.publish_audio_devices().await,
            Err(err) => self.emit(Event::CommandFailed(err)),
        }
    }

    pub(super) async fn set_noise_suppression(&mut self, mode: NoiseSuppression) {
        if let Err(err) = self.backend.set_noise_suppression(mode).await {
            self.emit(Event::CommandFailed(err));
        }
    }

    pub(super) async fn set_stream_volume(&mut self, percent: u8) {
        if let Err(err) = self.backend.set_stream_volume(percent.min(100)).await {
            self.emit(Event::CommandFailed(err));
        }
    }

    pub(super) async fn set_participant_volume(&mut self, identity: String, percent: u16) {
        let clamped = self.participant_volumes.set(&identity, percent);
        if let Err(err) = self
            .backend
            .set_participant_volume(&identity, clamped)
            .await
        {
            self.emit(Event::CommandFailed(err));
        }
    }

    pub(super) async fn set_noise_gate(&mut self, threshold: Option<i8>) {
        let threshold = threshold.map(|db| db.clamp(NOISE_GATE_MIN_DB, NOISE_GATE_MAX_DB));
        if let Err(err) = self.backend.set_noise_gate(threshold).await {
            self.emit(Event::CommandFailed(err));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{AudioDevices, Command, Event, SessionError};
    use crate::audio::cue::Cue;
    use crate::backend::BackendEvent;
    use crate::testing::BackendCall;

    use super::super::test_support::{Harness, device, harness};

    fn with_devices(h: &Harness) {
        h.backend
            .set_audio_devices(vec![device("mic1"), device("mic2")], vec![device("out1")]);
    }

    #[tokio::test(start_paused = true)]
    async fn mute_and_deafen_reach_backend_and_survive_room_switch() {
        let mut h = harness();
        h.send(Command::SetMuted(true));
        h.join("geral").await;
        h.send(Command::SetDeafened(true));
        h.send(Command::Join {
            room: "jogos".to_owned(),
        });
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { room, .. } if room == "jogos"));

        let calls = h.backend.calls();
        assert_eq!(
            &calls[calls.len() - 4..],
            [
                BackendCall::Disconnect,
                BackendCall::Connect {
                    url: "ws://fake.invalid".to_owned(),
                    token: "token-jogos-2".to_owned()
                },
                BackendCall::SetMicrophoneMuted(true),
                BackendCall::SetOutputMuted(true),
            ]
        );
        assert!(calls.contains(&BackendCall::SetOutputMuted(true)));
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_audio_devices_emits_the_list_outside_a_room() {
        let mut h = harness();
        with_devices(&h);
        h.send(Command::RefreshAudioDevices);
        assert_eq!(
            h.next().await,
            Event::AudioDevicesUpdated(AudioDevices {
                inputs: vec![device("mic1"), device("mic2")],
                outputs: vec![device("out1")],
                selected_input: None,
                selected_output: None,
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn devices_chosen_before_join_are_applied_on_connect() {
        let mut h = harness();
        with_devices(&h);
        h.send(Command::SetInputDevice(Some("mic2".to_owned())));
        h.send(Command::SetOutputDevice(Some("out1".to_owned())));
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        h.join("geral").await;
        assert_eq!(
            h.backend.applied_devices(),
            vec![(Some("mic2".to_owned()), Some("out1".to_owned()))]
        );
        let calls = h.backend.calls();
        let connect = calls
            .iter()
            .position(|c| matches!(c, BackendCall::Connect { .. }));
        let set = calls
            .iter()
            .position(|c| matches!(c, BackendCall::SetOutputDevice(_)));
        assert!(set < connect);
    }

    #[tokio::test(start_paused = true)]
    async fn device_change_in_a_room_reaches_the_backend_and_emits_the_update() {
        let mut h = harness();
        with_devices(&h);
        h.join("geral").await;
        h.send(Command::SetInputDevice(Some("mic1".to_owned())));
        let Event::AudioDevicesUpdated(devices) = h.next().await else {
            panic!("expected AudioDevicesUpdated");
        };
        assert_eq!(devices.selected_input.as_deref(), Some("mic1"));
        assert!(
            h.backend
                .calls()
                .contains(&BackendCall::SetInputDevice(Some("mic1".to_owned())))
        );
        assert!(h.backend.is_connected());

        h.send(Command::SetInputDevice(None));
        let Event::AudioDevicesUpdated(devices) = h.next().await else {
            panic!("expected AudioDevicesUpdated");
        };
        assert_eq!(devices.selected_input, None);
    }

    #[tokio::test(start_paused = true)]
    async fn device_choice_survives_room_switch_and_reconnect() {
        let mut h = harness();
        with_devices(&h);
        h.send(Command::SetOutputDevice(Some("out1".to_owned())));
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        h.join("geral").await;
        h.send(Command::Join {
            room: "jogos".to_owned(),
        });
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { .. }));
        assert!(matches!(
            h.next().await,
            Event::ChatHistory { before: None, .. }
        ));
        h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        });
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert!(matches!(h.next().await, Event::ParticipantList(_)));
        let applied = h.backend.applied_devices();
        assert_eq!(applied.len(), 3);
        assert!(
            applied
                .iter()
                .all(|(input, output)| input.is_none() && output.as_deref() == Some("out1"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_device_falls_back_to_default_without_failing_the_join() {
        let mut h = harness();
        with_devices(&h);
        h.send(Command::SetInputDevice(Some("unplugged".to_owned())));
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        h.join("geral").await;
        assert_eq!(h.backend.applied_devices(), vec![(None, None)]);
        assert!(h.backend.is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn backend_error_on_device_change_becomes_command_failed() {
        let mut h = harness();
        with_devices(&h);
        h.backend
            .fail_next_device_change(SessionError::Internal("boom".to_owned()));
        h.send(Command::SetOutputDevice(Some("out1".to_owned())));
        assert_eq!(
            h.next().await,
            Event::CommandFailed(SessionError::Internal("boom".to_owned()))
        );
        assert!(h.events.try_recv().is_err());

        h.send(Command::RefreshAudioDevices);
        let Event::AudioDevicesUpdated(devices) = h.next().await else {
            panic!("expected AudioDevicesUpdated");
        };
        assert_eq!(devices.selected_output, None);
    }

    #[tokio::test(start_paused = true)]
    async fn noise_gate_command_is_clamped_and_mic_level_is_forwarded() {
        let mut h = harness();
        h.join("geral").await;
        h.send(Command::SetNoiseGate(Some(-90)));
        h.send(Command::SetNoiseGate(Some(-5)));
        h.send(Command::SetNoiseGate(Some(-45)));
        h.send(Command::SetNoiseGate(None));
        h.send(Command::RefreshAudioDevices);
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        let gate_calls: Vec<_> = h
            .backend
            .calls()
            .into_iter()
            .filter(|c| matches!(c, BackendCall::SetNoiseGate(_)))
            .collect();
        assert_eq!(
            gate_calls,
            [
                BackendCall::SetNoiseGate(Some(-70)),
                BackendCall::SetNoiseGate(Some(-20)),
                BackendCall::SetNoiseGate(Some(-45)),
                BackendCall::SetNoiseGate(None),
            ]
        );

        h.backend.emit(BackendEvent::MicLevel {
            level_db: -33,
            gate_open: true,
        });
        assert_eq!(
            h.next().await,
            Event::MicLevel {
                level_db: -33,
                gate_open: true
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn play_cue_reaches_backend_inside_and_outside_a_room() {
        let mut h = harness();
        h.send(Command::PlayCue(Cue::Muted));
        h.send(Command::PlayCue(Cue::Unmuted));
        h.join("geral").await;
        h.send(Command::PlayCue(Cue::Deafened));
        h.send(Command::PlayCue(Cue::Undeafened));
        h.send(Command::RefreshAudioDevices);
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        let cues: Vec<_> = h
            .backend
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                BackendCall::PlayCue(cue) => Some(cue),
                _ => None,
            })
            .collect();
        assert_eq!(
            cues,
            vec![Cue::Muted, Cue::Unmuted, Cue::Deafened, Cue::Undeafened]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn participant_volume_clamps_works_outside_room_and_survives_room_switch_and_reconnect() {
        let mut h = harness();
        h.send(Command::SetParticipantVolume {
            identity: "alice".to_owned(),
            percent: 250,
        });
        h.send(Command::SetParticipantVolume {
            identity: "bob".to_owned(),
            percent: 60,
        });
        h.join("geral").await;
        assert_eq!(h.backend.participant_volume_gain("alice"), Some(2.0));
        assert_eq!(h.backend.participant_volume_gain("bob"), Some(0.6));

        h.send(Command::SetParticipantVolume {
            identity: "bob".to_owned(),
            percent: 100,
        });
        h.send(Command::SetParticipantVolume {
            identity: "carol".to_owned(),
            percent: 0,
        });
        h.send(Command::RefreshAudioDevices);
        assert!(matches!(h.next().await, Event::AudioDevicesUpdated(_)));
        assert_eq!(h.backend.participant_volume_gain("bob"), None);
        assert_eq!(h.backend.participant_volume_gain("carol"), Some(0.0));

        // Simulate disconnect + automatic reconnect: 100% ("bob") stays omitted, others reapply.
        let calls_before_reconnect = h.backend.calls().len();
        assert!(h.backend.emit(BackendEvent::Disconnected {
            reason: "dropped".to_owned(),
        }));
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert_eq!(h.next().await, Event::ParticipantList(Vec::new()));
        let reconnect_calls = &h.backend.calls()[calls_before_reconnect..];
        assert_eq!(
            reconnect_calls,
            [
                BackendCall::Connect {
                    url: "ws://fake.invalid".to_owned(),
                    token: "token-geral-2".to_owned(),
                },
                BackendCall::SetMicrophoneMuted(false),
                BackendCall::SetOutputMuted(false),
                BackendCall::SetParticipantVolume {
                    identity: "alice".to_owned(),
                    percent: 200,
                },
                BackendCall::SetParticipantVolume {
                    identity: "carol".to_owned(),
                    percent: 0,
                },
            ]
        );

        // Switch room: active entries survive room switch too.
        let calls_before_switch = h.backend.calls().len();
        h.send(Command::Join {
            room: "jogos".to_owned(),
        });
        assert_eq!(h.next().await, Event::Left);
        assert_eq!(h.next().await, Event::Connecting);
        assert!(matches!(h.next().await, Event::Joined { room, .. } if room == "jogos"));
        let switch_calls = &h.backend.calls()[calls_before_switch..];
        assert_eq!(
            switch_calls,
            [
                BackendCall::Disconnect,
                BackendCall::Connect {
                    url: "ws://fake.invalid".to_owned(),
                    token: "token-jogos-3".to_owned(),
                },
                BackendCall::SetMicrophoneMuted(false),
                BackendCall::SetOutputMuted(false),
                BackendCall::SetParticipantVolume {
                    identity: "alice".to_owned(),
                    percent: 200,
                },
                BackendCall::SetParticipantVolume {
                    identity: "carol".to_owned(),
                    percent: 0,
                },
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn server_initiated_mute_updates_local_state_without_fighting() {
        let mut h = harness();
        h.join("geral").await;
        let calls_before = h.backend.calls().len();

        assert!(h.backend.emit(BackendEvent::MuteChanged {
            identity: "local".to_owned(),
            muted: true,
        }));
        assert_eq!(
            h.next().await,
            Event::ParticipantMuteChanged {
                identity: "local".to_owned(),
                muted: true,
            }
        );
        // Must not call SetMicrophoneMuted back against the server.
        assert_eq!(h.backend.calls().len(), calls_before);

        // Reconnect applies the updated `muted = true` state.
        assert!(h.backend.emit(BackendEvent::Disconnected {
            reason: "network".to_owned(),
        }));
        assert_eq!(h.next().await, Event::Reconnecting);
        assert_eq!(h.next().await, Event::Reconnected);
        assert_eq!(h.next().await, Event::ParticipantList(Vec::new()));
        assert!(h.backend.calls()[calls_before..].contains(&BackendCall::SetMicrophoneMuted(true)));
    }
}
