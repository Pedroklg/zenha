//! Versões semânticas e a decisão de mostrar ou não uma atualização.

use std::fmt;

use super::AvailableUpdate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub fn parse_semver(raw: &str) -> Option<SemVer> {
    let trimmed = raw.trim();
    let without_prefix = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);

    // Build metadata (+...) is ignored; prereleases (-...) are rejected.
    let core = without_prefix.split('+').next()?;
    if core.contains('-') {
        return None;
    }
    let mut parts = core.split('.');
    let major = parse_component(parts.next()?)?;
    let minor = parse_component(parts.next()?)?;
    let patch = parse_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }

    Some(SemVer {
        major,
        minor,
        patch,
    })
}

fn parse_component(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse::<u64>().ok()
}
pub fn should_show_update(update: &AvailableUpdate, dismissed_version: Option<&str>) -> bool {
    let Some(latest) = parse_semver(&update.version) else {
        return false;
    };
    let Some(dismissed) = dismissed_version.and_then(parse_semver) else {
        return true;
    };
    latest > dismissed
}

pub fn next_demo_version(current_version: &str) -> String {
    match parse_semver(current_version) {
        Some(ver) => SemVer {
            major: ver.major,
            minor: ver.minor.saturating_add(1),
            patch: 0,
        }
        .to_string(),
        None => "0.2.0".to_owned(),
    }
}
