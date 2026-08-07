//! `download`: fetch a file over HTTP into the target path. Missing
//! parent directories are created automatically. Typically used to
//! stage a release bundle for later `copy_files` / `db_migrate` steps.

use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;
use tracing::info;

pub struct Download {
    url: String,
    dest: String,
}

#[derive(Deserialize)]
struct DownloadRaw {
    url: String,
    dest: String,
}

impl Download {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: DownloadRaw = body.try_into().context("invalid download config")?;
        Ok(Self {
            url: raw.url,
            dest: raw.dest,
        })
    }
}

impl Step for Download {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let url = &self.url;
            let dest = Path::new(&self.dest);

            info!("[{}] downloading {} to {}", id, url, dest.display());

            if let Some(parent) = dest.parent()
                && !parent.as_os_str().is_empty()
            {
                tokio::fs::create_dir_all(parent).await.with_context(|| {
                    format!("step '{}': failed to create {}", id, parent.display())
                })?;
            }

            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .with_context(|| format!("step '{}': failed to build HTTP client", id))?;

            let resp = client
                .get(url)
                .send()
                .await
                .with_context(|| format!("step '{}': failed to GET {}", id, url))?;

            let status = resp.status();
            if !status.is_success() {
                bail!("step '{}': HTTP {} for {}", id, status, url);
            }

            let bytes = resp
                .bytes()
                .await
                .with_context(|| format!("step '{}': failed to read body of {}", id, url))?;

            tokio::fs::write(dest, &bytes).await.with_context(|| {
                format!("step '{}': failed to write {}", id, dest.display())
            })?;

            info!(
                "[{}] wrote {} bytes to {}",
                id,
                bytes.len(),
                dest.display()
            );
            Ok(StepOutcome::default())
        })
    }
}
