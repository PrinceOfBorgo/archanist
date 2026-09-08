//! Shared backup/restore plumbing for file-mutating steps.
//!
//! Steps that overwrite or create files on disk use this to snapshot the exact
//! paths they touch *before* touching them, so `archanist rollback` can put
//! the filesystem back the way it was.
//!
//! Backups live under
//! `<base_dir>/.archanist-backups/<component>/<step_id>/` and are keyed
//! by step id alone - each new attempt for a step wipes and recreates
//! its directory. This mirrors the state model, which retains only the
//! most recent attempt: after a successful update the backup still holds
//! the pre-update files, so `rollback` can undo even a *successful* run
//! until the next attempt overwrites it.
//!
//! A [`BackupManifest`] is serialized into the step's `StepOutcome`
//! payload; `rollback` deserializes it and calls [`restore`].

use crate::steps::StepCtx;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Record of what a backing-up step saved during `apply`, persisted as
/// the step's payload and consumed by `rollback`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackupManifest {
    /// The backup directory holding the saved copies (informational).
    pub dir: String,
    /// One entry per destination path the step wrote.
    pub entries: Vec<BackupEntry>,
}

impl BackupManifest {
    /// True when nothing was recorded - lets `has_rollback` stay honest.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One path the step wrote, plus how to undo it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    /// The path the step wrote to.
    pub original: String,
    /// The saved copy to restore from, or `None` if `original` did not
    /// exist before `apply` (in which case rollback deletes it).
    pub backup: Option<String>,
}

/// `<base_dir>/.archanist-backups/<component>/<step_id>/`.
pub fn backup_dir(ctx: &StepCtx) -> PathBuf {
    ctx.base_dir
        .join(".archanist-backups")
        .join(&ctx.component)
        .join(&ctx.step_id)
}

/// Wipe and recreate the step's backup directory, returning it. Called at
/// the start of a backing-up `apply` so each attempt starts clean.
pub async fn reset(ctx: &StepCtx) -> Result<PathBuf> {
    let dir = backup_dir(ctx);
    if tokio::fs::try_exists(&dir).await.unwrap_or(false) {
        tokio::fs::remove_dir_all(&dir)
            .await
            .with_context(|| format!("failed to clear backup dir {}", dir.display()))?;
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("failed to create backup dir {}", dir.display()))?;
    Ok(dir)
}

/// Copies about-to-be-written files into the backup directory, assigning
/// each a unique numbered filename, and accumulates a [`BackupManifest`].
pub struct BackupBuilder {
    dir: PathBuf,
    next: usize,
    entries: Vec<BackupEntry>,
}

impl BackupBuilder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            next: 0,
            entries: Vec::new(),
        }
    }

    /// Record a path the step is about to write. If it currently exists,
    /// copy it into the backup dir (rollback restores it); otherwise mark
    /// it new (rollback deletes it). Call this BEFORE writing `original`.
    pub async fn record(&mut self, original: &Path) -> Result<()> {
        let exists = tokio::fs::try_exists(original).await.unwrap_or(false);
        let backup = if exists {
            let name = self.next.to_string();
            self.next += 1;
            let dest = self.dir.join(&name);
            tokio::fs::copy(original, &dest).await.with_context(|| {
                format!(
                    "failed to back up {} to {}",
                    original.display(),
                    dest.display()
                )
            })?;
            Some(dest.to_string_lossy().into_owned())
        } else {
            None
        };
        self.entries.push(BackupEntry {
            original: original.to_string_lossy().into_owned(),
            backup,
        });
        Ok(())
    }

    pub fn finish(self) -> BackupManifest {
        BackupManifest {
            dir: self.dir.to_string_lossy().into_owned(),
            entries: self.entries,
        }
    }
}

/// Restore every entry, newest first: entries with a saved copy are
/// copied back over the original; entries with no backup (files the step
/// created fresh) are removed.
pub async fn restore(manifest: &BackupManifest) -> Result<()> {
    for entry in manifest.entries.iter().rev() {
        let original = Path::new(&entry.original);
        match &entry.backup {
            Some(backup) => {
                if let Some(parent) = original.parent()
                    && !parent.as_os_str().is_empty()
                {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                tokio::fs::copy(backup, original).await.with_context(|| {
                    format!("failed to restore {} from {}", original.display(), backup)
                })?;
            }
            None => {
                if tokio::fs::try_exists(original).await.unwrap_or(false) {
                    tokio::fs::remove_file(original)
                        .await
                        .with_context(|| format!("failed to remove {}", original.display()))?;
                }
            }
        }
    }
    Ok(())
}

/// Serialize a manifest into a step payload value.
pub fn to_payload(manifest: &BackupManifest) -> Result<toml::Value> {
    toml::Value::try_from(manifest).context("failed to serialize backup manifest")
}

/// Deserialize a manifest from a step payload value. A payload that
/// doesn't match (e.g. from a `backup = false` run) yields an empty
/// manifest, so rollback is a safe no-op.
pub fn from_payload(payload: &toml::Value) -> BackupManifest {
    payload.clone().try_into().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(base: &Path) -> StepCtx {
        StepCtx {
            step_id: "s1".into(),
            component: "c1".into(),
            base_dir: base.to_path_buf(),
            is_self_update: false,
        }
    }

    #[tokio::test]
    async fn backs_up_and_restores_an_overwritten_file() {
        let tmp = std::env::temp_dir().join(format!("arch-bk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let target = tmp.join("file.txt");
        std::fs::write(&target, "ORIGINAL").unwrap();

        let c = ctx(&tmp);
        let dir = reset(&c).await.unwrap();
        let mut b = BackupBuilder::new(dir);
        b.record(&target).await.unwrap();
        let manifest = b.finish();

        // Simulate the step overwriting the file.
        std::fs::write(&target, "MODIFIED").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "MODIFIED");

        restore(&manifest).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "ORIGINAL");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn deletes_newly_created_file_on_restore() {
        let tmp = std::env::temp_dir().join(format!("arch-bk-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let target = tmp.join("new.txt");

        let c = ctx(&tmp);
        let dir = reset(&c).await.unwrap();
        let mut b = BackupBuilder::new(dir);
        b.record(&target).await.unwrap(); // does not exist yet
        let manifest = b.finish();

        // Simulate the step creating the file.
        std::fs::write(&target, "CREATED").unwrap();
        assert!(target.exists());

        restore(&manifest).await.unwrap();
        assert!(!target.exists(), "newly-created file should be removed");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn payload_round_trips() {
        let m = BackupManifest {
            dir: "/tmp/x".into(),
            entries: vec![BackupEntry {
                original: "/a/b".into(),
                backup: Some("/tmp/x/0".into()),
            }],
        };
        let v = to_payload(&m).unwrap();
        let back = from_payload(&v);
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].original, "/a/b");
        assert_eq!(back.entries[0].backup.as_deref(), Some("/tmp/x/0"));
    }

    #[test]
    fn mismatched_payload_yields_empty_manifest() {
        let v = toml::Value::String("not a manifest".into());
        assert!(from_payload(&v).is_empty());
    }
}
