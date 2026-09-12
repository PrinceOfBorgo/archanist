//! `copy_files`: copy a file, or recursively mirror a directory tree,
//! from `src` to `dest`. Missing destination parents are created.
//! Files at the destination are overwritten.
//!
//! When `backup = true` (the default) every destination path the step
//! writes is snapshotted first, so `archanist rollback` restores
//! overwritten files and deletes ones this step created. Set
//! `backup = false` to skip this (rollback then becomes a no-op).

use crate::steps::backup::{self, BackupBuilder};
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;
use tracing::info;

pub struct CopyFiles {
    src: String,
    dest: String,
    backup: bool,
}

#[derive(Deserialize)]
struct CopyFilesRaw {
    src: String,
    dest: String,
    #[serde(default = "default_backup")]
    backup: bool,
}

fn default_backup() -> bool {
    true
}

impl CopyFiles {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: CopyFilesRaw = body.try_into().context("invalid copy_files config")?;
        Ok(Self {
            src: raw.src,
            dest: raw.dest,
            backup: raw.backup,
        })
    }
}

impl Step for CopyFiles {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let src = Path::new(&self.src);
            let dest = Path::new(&self.dest);

            info!("[{}] copying {} to {}", id, src.display(), dest.display());

            let meta = tokio::fs::metadata(src).await.with_context(|| {
                format!("step '{}': source not accessible: {}", id, src.display())
            })?;

            // Snapshot the destination paths we're about to write before we
            // touch them (only when backups are enabled).
            let mut builder = if self.backup {
                Some(BackupBuilder::new(backup::reset(ctx).await?))
            } else {
                None
            };

            if meta.is_dir() {
                copy_dir_recursive(src, dest, builder.as_mut())
                    .await
                    .with_context(|| {
                        format!(
                            "step '{}': failed to copy directory {} to {}",
                            id,
                            src.display(),
                            dest.display()
                        )
                    })?;
            } else if meta.is_file() {
                if let Some(parent) = dest.parent()
                    && !parent.as_os_str().is_empty()
                {
                    tokio::fs::create_dir_all(parent).await.with_context(|| {
                        format!("step '{}': failed to create {}", id, parent.display())
                    })?;
                }
                if let Some(b) = builder.as_mut() {
                    b.record(dest).await.with_context(|| {
                        format!("step '{}': failed to back up {}", id, dest.display())
                    })?;
                }
                tokio::fs::copy(src, dest).await.with_context(|| {
                    format!(
                        "step '{}': failed to copy {} to {}",
                        id,
                        src.display(),
                        dest.display()
                    )
                })?;
            } else {
                bail!(
                    "step '{}': source {} is neither a file nor a directory",
                    id,
                    src.display()
                );
            }

            let mut outcome = StepOutcome::default();
            if let Some(b) = builder {
                outcome.payload = backup::to_payload(&b.finish())?;
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
            info!(
                "[{}] restoring {} file(s) from backup",
                ctx.step_id,
                manifest.entries.len()
            );
            backup::restore(&manifest)
                .await
                .with_context(|| format!("step '{}': failed to restore backup", ctx.step_id))
        })
    }

    fn has_rollback(&self) -> bool {
        self.backup
    }
}

async fn copy_dir_recursive(
    src: &Path,
    dest: &Path,
    mut builder: Option<&mut BackupBuilder>,
) -> Result<()> {
    tokio::fs::create_dir_all(dest).await?;
    let mut stack = vec![(src.to_path_buf(), dest.to_path_buf())];
    while let Some((s, d)) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&s).await?;
        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let dest_path = d.join(entry.file_name());
            let ft = entry.file_type().await?;
            if ft.is_dir() {
                tokio::fs::create_dir_all(&dest_path).await?;
                stack.push((src_path, dest_path));
            } else {
                if let Some(b) = builder.as_deref_mut() {
                    b.record(&dest_path).await?;
                }
                tokio::fs::copy(&src_path, &dest_path).await?;
            }
        }
    }
    Ok(())
}
