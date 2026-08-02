use crate::config::StepConfig;
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;
use tracing::info;

pub struct CopyFiles {
    id: String,
    src: String,
    dest: String,
}

#[derive(Deserialize)]
struct CopyFilesRaw {
    src: String,
    dest: String,
}

impl Step for CopyFiles {
    fn from_config(cfg: &StepConfig) -> Result<Self> {
        let raw: CopyFilesRaw = toml::Value::Table(cfg.extra.clone())
            .try_into()
            .with_context(|| format!("step '{}': invalid copy_files config", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            src: raw.src,
            dest: raw.dest,
        })
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &str {
        "copy_files"
    }

    fn apply<'a>(&'a self, _ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let src = Path::new(&self.src);
            let dest = Path::new(&self.dest);

            info!(
                "[{}] copying {} to {}",
                self.id,
                src.display(),
                dest.display()
            );

            let meta = tokio::fs::metadata(src).await.with_context(|| {
                format!(
                    "step '{}': source not accessible: {}",
                    self.id,
                    src.display()
                )
            })?;

            if meta.is_dir() {
                copy_dir_recursive(src, dest).await.with_context(|| {
                    format!(
                        "step '{}': failed to copy directory {} to {}",
                        self.id,
                        src.display(),
                        dest.display()
                    )
                })?;
            } else if meta.is_file() {
                if let Some(parent) = dest.parent()
                    && !parent.as_os_str().is_empty()
                {
                    tokio::fs::create_dir_all(parent).await.with_context(|| {
                        format!("step '{}': failed to create {}", self.id, parent.display())
                    })?;
                }
                tokio::fs::copy(src, dest).await.with_context(|| {
                    format!(
                        "step '{}': failed to copy {} to {}",
                        self.id,
                        src.display(),
                        dest.display()
                    )
                })?;
            } else {
                bail!(
                    "step '{}': source {} is neither a file nor a directory",
                    self.id,
                    src.display()
                );
            }
            Ok(StepOutcome::default())
        })
    }
}

async fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
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
                tokio::fs::copy(&src_path, &dest_path).await?;
            }
        }
    }
    Ok(())
}
