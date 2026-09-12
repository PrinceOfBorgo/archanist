//! `config_merge`: deep-merge a TOML `patch` file into a `target` file.
//!
//! Uses [`toml_edit`] so target comments and key ordering are preserved.
//! Values in `patch` overwrite values in `target`; nested tables
//! recurse; missing keys are added.
//!
//! When `backup = true` (the default) the `target` is snapshotted before
//! the merged result is written, so `archanist rollback` restores the
//! pre-merge file. Set `backup = false` to skip this (rollback then
//! becomes a no-op).

use crate::steps::backup::{self, BackupBuilder};
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use toml_edit::{DocumentMut, Table};
use tracing::info;

pub struct ConfigMerge {
    target: String,
    patch: String,
    backup: bool,
}

#[derive(Deserialize)]
struct ConfigMergeRaw {
    target: String,
    patch: String,
    #[serde(default = "default_backup")]
    backup: bool,
}

fn default_backup() -> bool {
    true
}

impl ConfigMerge {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: ConfigMergeRaw = body.try_into().context("invalid config_merge config")?;
        Ok(Self {
            target: raw.target,
            patch: raw.patch,
            backup: raw.backup,
        })
    }
}

impl Step for ConfigMerge {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let target = Path::new(&self.target);
            let patch = Path::new(&self.patch);

            info!(
                "[{}] merging {} into {}",
                id,
                patch.display(),
                target.display()
            );

            let target_content = tokio::fs::read_to_string(target).await.with_context(|| {
                format!("step '{}': failed to read target {}", id, target.display())
            })?;
            let patch_content = tokio::fs::read_to_string(patch).await.with_context(|| {
                format!("step '{}': failed to read patch {}", id, patch.display())
            })?;

            let mut target_doc: DocumentMut = target_content.parse().with_context(|| {
                format!(
                    "step '{}': failed to parse target {} as TOML",
                    id,
                    target.display()
                )
            })?;
            let patch_doc: DocumentMut = patch_content.parse().with_context(|| {
                format!(
                    "step '{}': failed to parse patch {} as TOML",
                    id,
                    patch.display()
                )
            })?;

            merge_tables(target_doc.as_table_mut(), patch_doc.as_table());

            // Snapshot the target before overwriting it with the merge.
            let mut manifest = None;
            if self.backup {
                let mut b = BackupBuilder::new(backup::reset(ctx).await?);
                b.record(target).await.with_context(|| {
                    format!("step '{}': failed to back up {}", id, target.display())
                })?;
                manifest = Some(b.finish());
            }

            tokio::fs::write(target, target_doc.to_string())
                .await
                .with_context(|| {
                    format!("step '{}': failed to write merged {}", id, target.display())
                })?;
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
            info!("[{}] restoring merged file from backup", ctx.step_id);
            backup::restore(&manifest)
                .await
                .with_context(|| format!("step '{}': failed to restore backup", ctx.step_id))
        })
    }

    fn has_rollback(&self) -> bool {
        self.backup
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

#[cfg(test)]
mod tests {
    use super::merge_tables;
    use toml_edit::DocumentMut;

    fn merged(dst: &str, src: &str) -> String {
        let mut d: DocumentMut = dst.parse().unwrap();
        let s: DocumentMut = src.parse().unwrap();
        merge_tables(d.as_table_mut(), s.as_table());
        d.to_string()
    }

    #[test]
    fn scalar_is_replaced() {
        let out = merged("a = 1\n", "a = 2\n");
        assert!(out.contains("a = 2"), "out: {out}");
        assert!(!out.contains("a = 1"), "out: {out}");
    }

    #[test]
    fn missing_key_is_added() {
        let out = merged("a = 1\n", "b = 2\n");
        assert!(out.contains("a = 1"), "out: {out}");
        assert!(out.contains("b = 2"), "out: {out}");
    }

    #[test]
    fn nested_table_deep_merges() {
        let out = merged(
            "[db]\nhost = \"local\"\nport = 5432\n",
            "[db]\nhost = \"prod\"\n",
        );
        assert!(out.contains("host = \"prod\""), "out: {out}");
        assert!(out.contains("port = 5432"), "out: {out}");
    }

    #[test]
    fn missing_nested_key_is_added() {
        let out = merged("[db]\nhost = \"local\"\n", "[db]\npassword = \"s3cret\"\n");
        assert!(out.contains("host = \"local\""), "out: {out}");
        assert!(out.contains("password = \"s3cret\""), "out: {out}");
    }

    #[test]
    fn empty_patch_leaves_target_unchanged() {
        let out = merged("a = 1\nb = 2\n", "");
        assert!(out.contains("a = 1"), "out: {out}");
        assert!(out.contains("b = 2"), "out: {out}");
    }

    #[test]
    fn target_comments_preserved() {
        let out = merged("# keep me\na = 1\n", "a = 2\n");
        assert!(out.contains("# keep me"), "out: {out}");
        assert!(out.contains("a = 2"), "out: {out}");
    }
}
