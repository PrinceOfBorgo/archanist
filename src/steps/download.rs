use crate::config::StepConfig;
use crate::steps::{ExecuteFuture, Step};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use tracing::info;

pub struct Download {
    id: String,
    url: String,
    dest: PathBuf,
}

#[derive(Deserialize)]
struct DownloadRaw {
    url: String,
    dest: PathBuf,
}

impl Step for Download {
    fn from_config(cfg: &StepConfig) -> Result<Self> {
        let raw: DownloadRaw = toml::Value::Table(cfg.extra.clone())
            .try_into()
            .with_context(|| format!("step '{}': invalid download config", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            url: raw.url,
            dest: raw.dest,
        })
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &str {
        "download"
    }

    fn describe(&self) -> String {
        format!("download {} -> {}", self.url, self.dest.display())
    }

    fn execute(&self) -> ExecuteFuture<'_> {
        Box::pin(async move {
            info!(
                "[{}] downloading {} to {}",
                self.id,
                self.url,
                self.dest.display()
            );

            if let Some(parent) = self.dest.parent()
                && !parent.as_os_str().is_empty()
            {
                tokio::fs::create_dir_all(parent).await.with_context(|| {
                    format!("step '{}': failed to create {}", self.id, parent.display())
                })?;
            }

            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .with_context(|| format!("step '{}': failed to build HTTP client", self.id))?;

            let resp = client
                .get(&self.url)
                .send()
                .await
                .with_context(|| format!("step '{}': failed to GET {}", self.id, self.url))?;

            let status = resp.status();
            if !status.is_success() {
                bail!("step '{}': HTTP {} for {}", self.id, status, self.url);
            }

            let bytes = resp.bytes().await.with_context(|| {
                format!("step '{}': failed to read body of {}", self.id, self.url)
            })?;

            tokio::fs::write(&self.dest, &bytes)
                .await
                .with_context(|| {
                    format!(
                        "step '{}': failed to write {}",
                        self.id,
                        self.dest.display()
                    )
                })?;

            info!(
                "[{}] wrote {} bytes to {}",
                self.id,
                bytes.len(),
                self.dest.display()
            );
            Ok(())
        })
    }
}
