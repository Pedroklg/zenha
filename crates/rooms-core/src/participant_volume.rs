use std::collections::BTreeMap;

pub(crate) const DEFAULT_PARTICIPANT_VOLUME_PERCENT: u16 = 100;
pub(crate) const MAX_PARTICIPANT_VOLUME_PERCENT: u16 = 200;

pub(crate) fn clamp_volume_percent(percent: u16) -> u16 {
    percent.min(MAX_PARTICIPANT_VOLUME_PERCENT)
}

pub(crate) fn volume_gain(percent: u16) -> f64 {
    f64::from(clamp_volume_percent(percent)) / 100.0
}

/// Per-participant voice volume overrides (`0..=200`, omitting default `100`).
#[derive(Debug, Default, Clone)]
pub(crate) struct ParticipantVolumes {
    entries: BTreeMap<String, u16>,
}

impl ParticipantVolumes {
    pub(crate) fn set(&mut self, identity: &str, percent: u16) -> u16 {
        let clamped = clamp_volume_percent(percent);
        if clamped == DEFAULT_PARTICIPANT_VOLUME_PERCENT {
            self.entries.remove(identity);
        } else {
            self.entries.insert(identity.to_owned(), clamped);
        }
        clamped
    }

    pub(crate) fn gain_for(&self, identity: &str) -> Option<f64> {
        self.entries.get(identity).copied().map(volume_gain)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, u16)> {
        self.entries
            .iter()
            .map(|(identity, &percent)| (identity.as_str(), percent))
    }

    #[cfg(test)]
    pub(crate) fn percent_for(&self, identity: &str) -> u16 {
        self.entries
            .get(identity)
            .copied()
            .unwrap_or(DEFAULT_PARTICIPANT_VOLUME_PERCENT)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_values_above_200_and_removes_100() {
        let mut volumes = ParticipantVolumes::default();
        assert_eq!(volumes.set("alice", 75), 75);
        assert_eq!(volumes.set("bob", 250), 200);
        assert_eq!(volumes.set("carol", 0), 0);
        assert_eq!(volumes.percent_for("alice"), 75);
        assert_eq!(volumes.percent_for("bob"), 200);
        assert_eq!(volumes.percent_for("carol"), 0);
        assert_eq!(volumes.len(), 3);

        assert_eq!(volumes.set("alice", 100), 100);
        assert_eq!(volumes.gain_for("alice"), None);
        assert_eq!(volumes.percent_for("alice"), 100);
        assert_eq!(volumes.len(), 2);
    }

    #[test]
    fn converts_stored_percentages_to_linear_gains() {
        let mut volumes = ParticipantVolumes::default();
        volumes.set("zero", 0);
        volumes.set("low", 70);
        volumes.set("high", 200);
        assert_eq!(volumes.gain_for("zero"), Some(0.0));
        assert_eq!(volumes.gain_for("low"), Some(0.7));
        assert_eq!(volumes.gain_for("high"), Some(2.0));
        assert_eq!(volumes.gain_for("missing"), None);
    }
}
