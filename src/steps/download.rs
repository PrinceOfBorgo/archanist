use crate::config::StepConfig;
use crate::interp::interpolate;
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;
use tracing::info;

pub struct Download {
    id: String,
    url: String,
    dest: String,
}

#[derive(Deserialize)]
struct DownloadRaw {
    url: String,
    dest: String,
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

    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let url = interpolate(&self.url, &ctx.vars)
                .with_context(|| format!("step '{}': failed to interpolate url", self.id))?;
            let dest_str = interpolate(&self.dest, &ctx.vars)
                .with_context(|| format!("step '{}': failed to interpolate dest", self.id))?;
            let dest = Path::new(&dest_str);

            info!("[{}] downloading {} to {}", self.id, url, dest.display());

            if let Some(parent) = dest.parent()
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
                .get(&url)
                .send()
                .await
                .with_context(|| format!("step '{}': failed to GET {}", self.id, url))?;

            let status = resp.status();
            if !status.is_success() {
                bail!("step '{}': HTTP {} for {}", self.id, status, url);
            }

            let bytes = resp
                .bytes()
                .await
                .with_context(|| format!("step '{}': failed to read body of {}", self.id, url))?;

            tokio::fs::write(dest, &bytes).await.with_context(|| {
                format!("step '{}': failed to write {}", self.id, dest.display())
            })?;

            info!(
                "[{}] wrote {} bytes to {}",
                self.id,
                bytes.len(),
                dest.display()
            );
            Ok(StepOutcome::default())
        })
    }
}
