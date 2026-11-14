use std::fmt;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use serde::Deserialize;

pub const DEFAULT_RELEASES_REPO: &str = "Pedroklg/zenha";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const USER_AGENT: &str = "Zenha";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub url: String,
}

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

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
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

pub fn resolve_releases_repo(env_value: Option<&str>) -> &str {
    env_value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_RELEASES_REPO)
}

pub fn latest_release_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

fn is_safe_http_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.contains(char::is_whitespace) {
        return false;
    }
    trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .is_some_and(|rest| !rest.is_empty())
}

pub fn evaluate_release(
    current_version: &str,
    tag_name: &str,
    html_url: &str,
    prerelease: bool,
    draft: bool,
) -> Option<AvailableUpdate> {
    if prerelease || draft {
        return None;
    }
    let current = parse_semver(current_version)?;
    let latest = parse_semver(tag_name)?;
    if latest <= current {
        return None;
    }
    let url = html_url.trim();
    if !is_safe_http_url(url) {
        return None;
    }
    Some(AvailableUpdate {
        version: latest.to_string(),
        url: url.to_owned(),
    })
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

pub fn spawn_check(ctx: egui::Context) -> Option<mpsc::Receiver<AvailableUpdate>> {
    let repo = resolve_releases_repo(option_env!("ZENHA_RELEASES_REPO"));
    let url = latest_release_url(repo);
    let current_version = env!("CARGO_PKG_VERSION");

    let (tx, rx) = mpsc::channel();
    match std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            if let Some(update) = fetch_latest_release(&url, current_version)
                && tx.send(update).is_ok()
            {
                ctx.request_repaint();
            }
        }) {
        Ok(_) => Some(rx),
        Err(err) => {
            tracing::debug!(%err, "could not spawn update check thread");
            None
        }
    }
}

fn fetch_latest_release(url: &str, current_version: &str) -> Option<AvailableUpdate> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            tracing::debug!(%err, "could not initialize runtime for update check");
            return None;
        }
    };

    runtime.block_on(async {
        let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => client,
            Err(err) => {
                tracing::debug!(%err, "could not build update check http client");
                return None;
            }
        };

        let response = match client
            .get(url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                tracing::debug!(%err, "update check request failed");
                return None;
            }
        };

        let response = match response.error_for_status() {
            Ok(resp) => resp,
            Err(err) => {
                tracing::debug!(%err, "update check returned non-success status");
                return None;
            }
        };

        let release: GitHubRelease = match response.json().await {
            Ok(release) => release,
            Err(err) => {
                tracing::debug!(%err, "could not decode github release payload");
                return None;
            }
        };

        evaluate_release(
            current_version,
            &release.tag_name,
            &release.html_url,
            release.prerelease,
            release.draft,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_v_prefixed_versions() {
        assert_eq!(
            parse_semver("0.1.0"),
            Some(SemVer {
                major: 0,
                minor: 1,
                patch: 0
            })
        );
        assert_eq!(
            parse_semver("v1.2.3"),
            Some(SemVer {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        assert_eq!(
            parse_semver(" V10.20.30+build1 "),
            Some(SemVer {
                major: 10,
                minor: 20,
                patch: 30
            })
        );
    }

    #[test]
    fn ignores_prereleases_and_malformed_versions() {
        assert_eq!(parse_semver("v0.2.0-rc.1"), None);
        assert_eq!(parse_semver("0.2.0-alpha"), None);
        assert_eq!(parse_semver("1.0.0-beta+12"), None);
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("v1.2"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("+1.0.0"), None);
        assert_eq!(parse_semver("1.two.3"), None);
    }

    #[test]
    fn semver_ordering_compares_numerically() {
        let v0_2_0 = parse_semver("0.2.0");
        let v0_10_0 = parse_semver("0.10.0");
        assert!(v0_10_0 > v0_2_0);
    }

    #[test]
    fn evaluate_release_accepts_only_newer_stable_releases() {
        let update = evaluate_release(
            "0.1.0",
            "v0.2.0",
            "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
            false,
            false,
        );
        assert_eq!(
            update,
            Some(AvailableUpdate {
                version: "0.2.0".to_owned(),
                url: "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0".to_owned(),
            })
        );

        assert_eq!(
            evaluate_release(
                "0.2.0",
                "v0.2.0",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
                false,
                false,
            ),
            None
        );
        assert_eq!(
            evaluate_release(
                "0.3.0",
                "v0.2.9",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.9",
                false,
                false,
            ),
            None
        );
    }

    #[test]
    fn evaluate_release_ignores_prereleases_drafts_and_unsafe_urls() {
        assert_eq!(
            evaluate_release(
                "0.1.0",
                "v0.2.0",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
                true,
                false,
            ),
            None
        );
        assert_eq!(
            evaluate_release(
                "0.1.0",
                "v0.2.0",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
                false,
                true,
            ),
            None
        );
        assert_eq!(
            evaluate_release(
                "0.1.0",
                "v0.2.0-rc.1",
                "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0-rc.1",
                false,
                false,
            ),
            None
        );
        assert_eq!(
            evaluate_release("0.1.0", "v0.2.0", "file:///etc/passwd", false, false),
            None
        );
    }

    #[test]
    fn should_show_update_respects_dismissed_version_until_newer_arrives() {
        let v0_2_0 = AvailableUpdate {
            version: "0.2.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0".to_owned(),
        };
        let v0_3_0 = AvailableUpdate {
            version: "0.3.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.3.0".to_owned(),
        };

        assert!(should_show_update(&v0_2_0, None));
        assert!(!should_show_update(&v0_2_0, Some("0.2.0")));
        assert!(!should_show_update(&v0_2_0, Some("v0.2.0")));
        assert!(!should_show_update(&v0_2_0, Some("0.2.5")));
        assert!(should_show_update(&v0_3_0, Some("0.2.0")));
        assert!(should_show_update(&v0_2_0, Some("corrupted")));
    }

    #[test]
    fn resolves_releases_repo_with_default_fallback() {
        assert_eq!(resolve_releases_repo(None), "Pedroklg/zenha");
        assert_eq!(resolve_releases_repo(Some("")), "Pedroklg/zenha");
        assert_eq!(resolve_releases_repo(Some("  ")), "Pedroklg/zenha");
        assert_eq!(resolve_releases_repo(Some(" org/custom ")), "org/custom");
        assert_eq!(
            latest_release_url("org/custom"),
            "https://api.github.com/repos/org/custom/releases/latest"
        );
    }
}
