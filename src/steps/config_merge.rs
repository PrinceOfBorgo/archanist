use crate::config::StepConfig;
use crate::steps::{ExecuteFuture, Step};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Table};
use tracing::info;

pub struct ConfigMerge {
    id: String,
    target: PathBuf,
    patch: PathBuf,
}

#[derive(Deserialize)]
struct ConfigMergeRaw {
    target: PathBuf,
    patch: PathBuf,
}

impl Step for ConfigMerge {
    fn from_config(cfg: &StepConfig) -> Result<Self> {
        let raw: ConfigMergeRaw = toml::Value::Table(cfg.extra.clone())
            .try_into()
            .with_context(|| format!("step '{}': invalid config_merge config", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            target: raw.target,
            patch: raw.patch,
        })
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &str {
        "config_merge"
    }

    fn describe(&self) -> String {
        format!(
            "merge {} into {}",
            self.patch.display(),
            self.target.display()
        )
    }

    fn execute(&self) -> ExecuteFuture<'_> {
        Box::pin(async move {
            info!(
                "[{}] merging {} into {}",
                self.id,
                self.patch.display(),
                self.target.display()
            );

            let target_content =
                tokio::fs::read_to_string(&self.target)
                    .await
                    .with_context(|| {
                        format!(
                            "step '{}': failed to read target {}",
                            self.id,
                            self.target.display()
                        )
                    })?;
            let patch_content =
                tokio::fs::read_to_string(&self.patch)
                    .await
                    .with_context(|| {
                        format!(
                            "step '{}': failed to read patch {}",
                            self.id,
                            self.patch.display()
                        )
                    })?;

            let mut target_doc: DocumentMut = target_content.parse().with_context(|| {
                format!(
                    "step '{}': failed to parse target {} as TOML",
                    self.id,
                    self.target.display()
                )
            })?;
            let patch_doc: DocumentMut = patch_content.parse().with_context(|| {
                format!(
                    "step '{}': failed to parse patch {} as TOML",
                    self.id,
                    self.patch.display()
                )
            })?;

            merge_tables(target_doc.as_table_mut(), patch_doc.as_table());

            tokio::fs::write(&self.target, target_doc.to_string())
                .await
                .with_context(|| {
                    format!(
                        "step '{}': failed to write merged {}",
                        self.id,
                        self.target.display()
                    )
                })?;
            Ok(())
        })
    }
}

/// Deep-merge `src` into `dst`. Values in `src` overwrite values in `dst`.
/// Nested tables recurse; non-table values replace wholesale.
fn merge_tables(dst: &mut Table, src: &Table) {
    for (key, src_item) in src.iter() {
        match dst.get_mut(key) {
            Some(dst_item) => {
                if let (Some(dst_t), Some(src_t)) = (dst_item.as_table_mut(), src_item.as_table()) {
                    merge_tables(dst_t, src_t);
                } else {
                    *dst_item = src_item.clone();
                }
            }
            None => {
                dst.insert(key, src_item.clone());
            }
        }
    }
}
