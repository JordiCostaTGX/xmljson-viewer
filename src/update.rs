use serde::Deserialize;
use std::time::Duration;
use ureq::{
    Agent,
    tls::{RootCerts, TlsConfig},
};

pub const RELEASES_PAGE: &str = "https://github.com/JordiCostaTGX/xmljson-viewer/releases";
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/JordiCostaTGX/xmljson-viewer/releases/latest";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Automatic checks run at most this often; a manual check always runs.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
}

impl Release {
    /// Built from the version, never from the response, so only GitHub release pages are opened.
    pub fn url(&self) -> String {
        format!("{RELEASES_PAGE}/tag/v{}", self.version)
    }
}

/// `major.minor.patch`, with an optional leading `v`; pre-release tags are rejected.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut next = || parts.next()?.parse().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

pub fn should_check(enabled: bool, last_check_secs: u64, now_secs: u64) -> bool {
    enabled && now_secs.saturating_sub(last_check_secs) >= CHECK_INTERVAL.as_secs()
}

fn parse_latest(body: &str) -> Result<Release, String> {
    #[derive(Deserialize)]
    struct Latest {
        tag_name: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
    }
    let latest: Latest =
        serde_json::from_str(body).map_err(|error| format!("Unexpected response: {error}"))?;
    let (major, minor, patch) = parse_version(&latest.tag_name)
        .filter(|_| !latest.draft && !latest.prerelease)
        .ok_or_else(|| format!("Unexpected release tag {:?}", latest.tag_name))?;
    Ok(Release {
        version: format!("{major}.{minor}.{patch}"),
    })
}

/// Asks GitHub for the latest published release. Blocking; call it off the UI thread.
pub fn fetch_latest() -> Result<Release, String> {
    let agent: Agent = Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(format!("xmljson-viewer/{CURRENT_VERSION}"))
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into();
    let body = agent
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|error| error.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|error| error.to_string())?;
    parse_latest(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically_and_reject_unexpected_formats() {
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("1.2.5", "1.2.4"));
        assert!(!is_newer("v1.2.4", "1.2.4"));
        assert!(!is_newer("1.2.3", "1.2.4"));
        assert!(!is_newer("v2.0.0-beta", "1.2.4"));
        assert!(!is_newer("1.2", "1.2.4"));
        assert!(!is_newer("1.2.4.1", "1.2.4"));
    }

    #[test]
    fn checks_at_most_once_per_interval_and_only_when_enabled() {
        let day = CHECK_INTERVAL.as_secs();
        assert!(should_check(true, 0, day));
        assert!(!should_check(true, day, day + 10));
        assert!(should_check(true, day, 2 * day));
        assert!(!should_check(false, 0, 10 * day));
    }

    #[test]
    fn latest_release_uses_only_the_tag_and_skips_drafts_and_prereleases() {
        let release =
            parse_latest(r#"{"tag_name":"v1.3.0","html_url":"https://example.com","draft":false}"#)
                .unwrap();
        assert_eq!(release.version, "1.3.0");
        assert_eq!(
            release.url(),
            "https://github.com/JordiCostaTGX/xmljson-viewer/releases/tag/v1.3.0"
        );
        assert!(parse_latest(r#"{"tag_name":"v1.3.0","prerelease":true}"#).is_err());
        assert!(parse_latest(r#"{"tag_name":"latest"}"#).is_err());
        assert!(parse_latest("<html>").is_err());
    }

    #[test]
    #[ignore = "needs network access to GitHub"]
    fn fetches_the_latest_release_from_github() {
        let release = fetch_latest().unwrap();
        assert!(parse_version(&release.version).is_some());
    }
}
