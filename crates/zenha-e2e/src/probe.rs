//! Output audio probe: common interface and device matching.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::dsp::ToneMeasurement;
use crate::protocol::DeviceInfo;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub trait AudioProbe: Send + Sync {
    /// Captures the mix of the test render endpoint for `duration` and measures `hz` in it.
    fn measure_tone(&self, hz: f64, duration: Duration) -> anyhow::Result<ToneMeasurement>;
    /// `is_default` marks the test endpoint: the pinned one, else the Windows default.
    fn render_devices(&self) -> anyhow::Result<Vec<RenderDevice>>;

    /// True when the tests were pinned to a device other than the Windows default.
    fn pinned(&self) -> bool {
        false
    }

    fn default_render_device(&self) -> anyhow::Result<RenderDevice> {
        self.render_devices()?
            .into_iter()
            .find(|device| device.is_default)
            .ok_or_else(|| anyhow::anyhow!("no default render device"))
    }
}

/// Probe of platforms without loopback support.
pub struct UnavailableProbe;

impl AudioProbe for UnavailableProbe {
    fn measure_tone(&self, _hz: f64, _duration: Duration) -> anyhow::Result<ToneMeasurement> {
        anyhow::bail!("the audio probe is only available on Windows")
    }

    fn render_devices(&self) -> anyhow::Result<Vec<RenderDevice>> {
        anyhow::bail!("the audio probe is only available on Windows")
    }
}

fn normalize(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Finds, among the SDK output devices, the one that is the default Windows render endpoint.
///
/// Order: the SDK id contained in the endpoint id, equal names, then one name containing
/// the other (the SDK may prefix "Default - ").
pub fn match_sdk_output<'a>(
    default: &RenderDevice,
    outputs: &'a [DeviceInfo],
) -> Option<&'a DeviceInfo> {
    let endpoint_id = normalize(&default.id);
    let endpoint_name = normalize(&default.name);
    let by_id = outputs.iter().find(|device| {
        let id = normalize(&device.id);
        !id.is_empty() && !id.starts_with("index:") && endpoint_id.contains(&id)
    });
    let by_name = || {
        outputs
            .iter()
            .find(|device| normalize(&device.name) == endpoint_name)
    };
    let by_part = || {
        outputs.iter().find(|device| {
            let name = normalize(&device.name);
            !name.is_empty()
                && !endpoint_name.is_empty()
                && (name.contains(&endpoint_name) || endpoint_name.contains(&name))
        })
    };
    by_id.or_else(by_name).or_else(by_part)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(id: &str, name: &str) -> RenderDevice {
        RenderDevice {
            id: id.into(),
            name: name.into(),
            is_default: true,
        }
    }

    fn sdk(id: &str, name: &str) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            name: name.into(),
        }
    }

    #[test]
    fn matches_by_guid_inside_the_endpoint_id() {
        let default = endpoint(
            "{0.0.0.00000000}.{ABCD-1234}",
            "Speakers (Realtek(R) Audio)",
        );
        let outputs = [sdk("{other}", "Headset"), sdk("{abcd-1234}", "Whatever")];
        assert_eq!(
            match_sdk_output(&default, &outputs).map(|d| d.id.as_str()),
            Some("{abcd-1234}")
        );
    }

    #[test]
    fn falls_back_to_the_name_and_ignores_index_ids() {
        let default = endpoint("{0.0.0.00000000}.{x}", "Speakers (Realtek(R) Audio)");
        let outputs = [
            sdk("index:0", "Headset"),
            sdk("index:1", "Speakers (Realtek(R) Audio)"),
        ];
        assert_eq!(
            match_sdk_output(&default, &outputs).map(|d| d.id.as_str()),
            Some("index:1")
        );
    }

    #[test]
    fn partial_name_matches_and_unrelated_names_do_not() {
        let default = endpoint("{0.0.0.0}.{x}", "Speakers (Realtek)");
        let outputs = [sdk("g", "Default - Speakers (Realtek)")];
        assert!(match_sdk_output(&default, &outputs).is_some());
        let unrelated = [sdk("g", "Monitor (HDMI)")];
        assert!(match_sdk_output(&default, &unrelated).is_none());
        assert!(match_sdk_output(&default, &[]).is_none());
    }

    #[test]
    fn unavailable_probe_errors() {
        assert!(
            UnavailableProbe
                .measure_tone(1000.0, Duration::from_millis(10))
                .is_err()
        );
        assert!(UnavailableProbe.default_render_device().is_err());
    }
}
