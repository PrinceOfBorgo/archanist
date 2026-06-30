use crate::config::StepConfig;
use crate::interp::{Env, interpolate};
use crate::steps::{ExecuteFuture, Step};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use toml_edit::{DocumentMut, Table};
use tracing::info;

pub struct ConfigMerge {
    id: String,
    target: String,
    patch: String,
}

#[derive(Deserialize)]
struct ConfigMergeRaw {
    target: String,
    patch: String,
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
        format!("merge {} into {}", self.patch, self.target)
    }

    fn execute<'a>(&'a self, env: &'a mut Env) -> ExecuteFuture<'a> {
        Box::pin(async move {
            let target_str = interpolate(&self.target, env)
                .with_context(|| format!("step '{}': failed to interpolate target", self.id))?;
            let patch_str = interpolate(&self.patch, env)
                .with_context(|| format!("step '{}': failed to interpolate patch", self.id))?;
            let target = Path::new(&target_str);
            let patch = Path::new(&patch_str);

            info!(
                "[{}] merging {} into {}",
                self.id,
                patch.display(),
                target.display()
            );

            let target_content = tokio::fs::read_to_string(target).await.with_context(|| {
                format!(
                    "step '{}': failed to read target {}",
                    self.id,
                    target.display()
                )
            })?;
            let patch_content = tokio::fs::read_to_string(patch).await.with_context(|| {
                format!(
                    "step '{}': failed to read patch {}",
                    self.id,
                    patch.display()
                )
            })?;

            let mut target_doc: DocumentMut = target_content.parse().with_context(|| {
                format!(
                    "step '{}': failed to parse target {} as TOML",
                    self.id,
                    target.display()
                )
            })?;
            let patch_doc: DocumentMut = patch_content.parse().with_context(|| {
                format!(
                    "step '{}': failed to parse patch {} as TOML",
                    self.id,
                    patch.display()
                )
            })?;

            merge_tables(target_doc.as_table_mut(), patch_doc.as_table());

            tokio::fs::write(target, target_doc.to_string())
                .await
                .with_context(|| {
                    format!(
                        "step '{}': failed to write merged {}",
                        self.id,
                        target.display()
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
