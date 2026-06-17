use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::Duration;
use tracing::debug;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ReleaseSource {
    Github { repo: String },
}

impl ReleaseSource {
    pub async fn fetch_latest(&self) -> Result<String> {
        match self {
            Self::Github { repo } => fetch_github_latest(repo).await,
        }
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
}

async fn fetch_github_latest(repo: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
    debug!("querying github release: {}", url);

    let client = reqwest::Client::builder()
        .user_agent(concat!("archanist/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")?;

    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to GET {}", url))?;

    let status = resp.status();
    if !status.is_success() {
        bail!("github release query for '{}' returned HTTP {}", repo, status);
    }

    let raw: GithubRelease = resp
        .json()
        .await
        .with_context(|| format!("failed to parse github release response for '{}'", repo))?;

    let version = raw
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&raw.tag_name)
        .to_string();
    Ok(version)
}
