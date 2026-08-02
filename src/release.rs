use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;
use std::time::Duration;
use tracing::debug;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReleaseSource {
    /// Latest non-draft, non-prerelease GitHub release for `repo` (owner/name).
    Github { repo: String },
    /// Same as `Github` but derives owner/name from a `ghcr.io/owner/name[:tag]`
    /// image reference - handy for components that already declare their
    /// container image and want release checks to follow the same GitHub org.
    GhcrAuto { image: String },
    /// Highest semver-parseable tag from a Docker Hub repository.
    DockerHub { image: String },
    /// Fixed version - no upstream check. Useful for third-party artefacts
    /// where the archanist just applies a known-good version.
    Pinned { version: String },
}

impl ReleaseSource {
    /// Look up the latest available version. Returns `None` when no valid
    /// release is available (e.g. only drafts/prereleases on GitHub, no
    /// semver tags on Docker Hub).
    pub async fn fetch_latest(&self) -> Result<Option<String>> {
        match self {
            Self::Github { repo } => fetch_github(repo).await,
            Self::GhcrAuto { image } => {
                let repo = extract_repo_from_ghcr(image)?;
                fetch_github(&repo).await
            }
            Self::DockerHub { image } => fetch_docker_hub(image).await,
            Self::Pinned { version } => Ok(Some(version.clone())),
        }
    }
}

/// Compare two version strings using semver semantics, falling back to string
/// inequality when either isn't valid semver.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (Version::parse(latest), Version::parse(current)) {
        (Ok(l), Ok(c)) => l > c,
        _ => latest != current,
    }
}

fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("archanist/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

async fn fetch_github(repo: &str) -> Result<Option<String>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    debug!("querying github release: {}", url);
    let resp = build_client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("github release query for '{repo}' returned HTTP {status}");
    }
    let rel: GithubRelease = resp
        .json()
        .await
        .with_context(|| format!("failed to parse github release response for '{repo}'"))?;
    if rel.draft || rel.prerelease {
        debug!(
            "github release for '{}' is draft/prerelease, skipping",
            repo
        );
        return Ok(None);
    }
    Ok(Some(strip_v_prefix(&rel.tag_name).to_string()))
}

/// `ghcr.io/owner/name[:tag]` -> `owner/name`.
fn extract_repo_from_ghcr(image: &str) -> Result<String> {
    let without_host = image
        .strip_prefix("ghcr.io/")
        .with_context(|| format!("ghcr_auto: image '{image}' is not a ghcr.io reference"))?;
    let without_tag = without_host.split(':').next().unwrap_or(without_host);
    let mut parts = without_tag.split('/');
    let owner = parts
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("ghcr_auto: image '{image}' has no owner segment"))?;
    let name = parts
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("ghcr_auto: image '{image}' has no name segment"))?;
    Ok(format!("{owner}/{name}"))
}

#[derive(Deserialize)]
struct DockerHubTagList {
    results: Vec<DockerHubTag>,
}

#[derive(Deserialize)]
struct DockerHubTag {
    name: String,
}

async fn fetch_docker_hub(image: &str) -> Result<Option<String>> {
    // Docker Hub requires `library/name` for official images.
    let path = if image.contains('/') {
        image.to_string()
    } else {
        format!("library/{image}")
    };
    let url = format!("https://hub.docker.com/v2/repositories/{path}/tags/?page_size=100");
    debug!("querying docker hub tags: {}", url);
    let resp = build_client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("docker hub tag query for '{image}' returned HTTP {status}");
    }
    let list: DockerHubTagList = resp
        .json()
        .await
        .with_context(|| format!("failed to parse docker hub response for '{image}'"))?;
    let highest = list
        .results
        .iter()
        .filter_map(|t| Version::parse(strip_v_prefix(&t.name)).ok())
        .max();
    Ok(highest.map(|v| v.to_string()))
}

fn strip_v_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_semver_basic() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.1"));
        assert!(!is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn is_newer_semver_double_digit() {
        // The whole point of semver: string comparison would say v2 > v10.
        assert!(is_newer("10.0.0", "2.0.0"));
        assert!(is_newer("1.10.0", "1.2.0"));
    }

    #[test]
    fn is_newer_prerelease_ordering() {
        // Per semver, 2.0.0 > 2.0.0-alpha.
        assert!(is_newer("2.0.0", "2.0.0-alpha"));
        assert!(!is_newer("2.0.0-alpha", "2.0.0"));
    }

    #[test]
    fn is_newer_non_semver_falls_back_to_inequality() {
        assert!(is_newer("nightly-2026-07-27", "nightly-2026-07-26"));
        assert!(!is_newer("nightly", "nightly"));
    }

    #[test]
    fn extract_repo_from_ghcr_happy_path() {
        assert_eq!(
            extract_repo_from_ghcr("ghcr.io/owner/name").unwrap(),
            "owner/name"
        );
    }

    #[test]
    fn extract_repo_from_ghcr_ignores_tag() {
        assert_eq!(
            extract_repo_from_ghcr("ghcr.io/owner/name:v1.2.3").unwrap(),
            "owner/name"
        );
    }

    #[test]
    fn extract_repo_from_ghcr_ignores_extra_path_segments() {
        assert_eq!(
            extract_repo_from_ghcr("ghcr.io/owner/name/sub").unwrap(),
            "owner/name"
        );
    }

    #[test]
    fn extract_repo_from_ghcr_rejects_wrong_host() {
        let err = extract_repo_from_ghcr("docker.io/owner/name")
            .err()
            .unwrap();
        assert!(err.to_string().contains("not a ghcr.io reference"));
    }

    #[test]
    fn extract_repo_from_ghcr_rejects_missing_name() {
        let err = extract_repo_from_ghcr("ghcr.io/owner").err().unwrap();
        assert!(err.to_string().contains("no name segment"));
    }

    #[tokio::test]
    async fn pinned_returns_configured_version() {
        let src = ReleaseSource::Pinned {
            version: "3.2.1".into(),
        };
        assert_eq!(src.fetch_latest().await.unwrap(), Some("3.2.1".into()));
    }
}
