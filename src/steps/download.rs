//! `download`: fetch a file over HTTP into the target path. Missing
//! parent directories are created automatically. Typically used to
//! stage a release bundle for later `copy_files` / `db_migrate` steps.
//!
//! When `backup = true` (the default) the destination is snapshotted
//! before it is overwritten, so `archanist rollback` restores the prior
//! file or deletes a freshly-downloaded one. Set `backup = false` to skip
//! this (rollback then becomes a no-op).

use crate::steps::backup::{self, BackupBuilder};
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;
use tracing::info;

pub struct Download {
    url: String,
    dest: String,
    backup: bool,
}

#[derive(Deserialize)]
struct DownloadRaw {
    url: String,
    dest: String,
    #[serde(default = "default_backup")]
    backup: bool,
}

fn default_backup() -> bool {
    true
}

impl Download {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: DownloadRaw = body.try_into().context("invalid download config")?;
        Ok(Self {
            url: raw.url,
            dest: raw.dest,
            backup: raw.backup,
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

            // Snapshot the destination before overwriting it.
            let mut manifest = None;
            if self.backup {
                let mut b = BackupBuilder::new(backup::reset(ctx).await?);
                b.record(dest).await.with_context(|| {
                    format!("step '{}': failed to back up {}", id, dest.display())
                })?;
                manifest = Some(b.finish());
            }

            tokio::fs::write(dest, &bytes)
                .await
                .with_context(|| format!("step '{}': failed to write {}", id, dest.display()))?;

            info!("[{}] wrote {} bytes to {}", id, bytes.len(), dest.display());
            let mut outcome = StepOutcome::default();
            if let Some(m) = manifest {
                outcome.payload = backup::to_payload(&m)?;
            }
            Ok(outcome)
        })
    }

    fn rollback<'a>(
        &'a self,
        ctx: &'a StepCtx,
        payload: &'a toml::Value,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let manifest = backup::from_payload(payload);
            if manifest.is_empty() {
                return Ok(());
            }
            info!("[{}] restoring downloaded file from backup", ctx.step_id);
            backup::restore(&manifest)
                .await
                .with_context(|| format!("step '{}': failed to restore backup", ctx.step_id))
        })
    }

    fn has_rollback(&self) -> bool {
        self.backup
    }
}
