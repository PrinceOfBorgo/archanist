//! `db_migrate`: driver-agnostic forward-only migration runner.
//!
//! Migration files are discovered via `glob` **or** listed explicitly
//! in `files` (the two are mutually exclusive) and applied in
//! lexicographic order of file stem. Each file is passed to a
//! user-supplied `command` template with the placeholders `{file}`
//! (full path) and `{name}` (file stem) substituted per invocation.

use crate::config::StepConfig;
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::info;

pub struct DbMigrate {
    id: String,
    source: Source,
    command: Vec<String>,
    cwd: Option<String>,
}

/// How migration files are discovered. Populated in [`DbMigrate::from_config`]
/// from the mutually-exclusive `glob` / `files` fields of the raw body.
enum Source {
    /// Filesystem glob pattern expanded at apply time. Errors if it
    /// matches no files.
    Glob(String),
    /// Explicit, pre-resolved list of paths.
    Files(Vec<String>),
}

/// Raw TOML shape of a `db_migrate` step body, before validation.
///
/// [`DbMigrate::from_config`] then enforces the invariants that a raw
/// deserialize can't express: exactly one of `glob` / `files` must be
/// set, and `command` must be non-empty.
#[derive(Deserialize)]
struct DbMigrateRaw {
    /// Filesystem glob pattern for discovering migration files.
    /// Mutually exclusive with [`Self::files`].
    #[serde(default)]
    glob: Option<String>,
    /// Explicit list of migration file paths.
    /// Mutually exclusive with [`Self::glob`].
    #[serde(default)]
    files: Option<Vec<String>>,
    /// Command template used to apply each migration. `{file}` and
    /// `{name}` (see [`render_argv`]) are substituted per invocation.
    /// Must contain at least one element (the program to run).
    command: Vec<String>,
    /// Optional working directory for the spawned migration process.
    #[serde(default)]
    cwd: Option<String>,
}

impl Step for DbMigrate {
    fn from_config(cfg: &StepConfig) -> Result<Self> {
        let raw: DbMigrateRaw = toml::Value::Table(cfg.extra.clone())
            .try_into()
            .with_context(|| format!("step '{}': invalid db_migrate config", cfg.id))?;

        if raw.command.is_empty() {
            bail!("step '{}': `command` must not be empty", cfg.id);
        }

        let source = match (raw.glob, raw.files) {
            (Some(g), None) => Source::Glob(g),
            (None, Some(f)) => Source::Files(f),
            (Some(_), Some(_)) => {
                bail!(
                    "step '{}': `glob` and `files` are mutually exclusive",
                    cfg.id
                )
            }
            (None, None) => bail!("step '{}': one of `glob` or `files` must be set", cfg.id),
        };

        Ok(Self {
            id: cfg.id.clone(),
            source,
            command: raw.command,
            cwd: raw.cwd,
        })
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &str {
        "db_migrate"
    }

    fn apply<'a>(&'a self, _ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let files: Vec<PathBuf> = match &self.source {
                Source::Glob(pattern) => {
                    info!("[{}] expanding glob: {}", self.id, pattern);
                    let mut matches: Vec<PathBuf> = glob::glob(pattern)
                        .with_context(|| format!("step '{}': invalid glob '{}'", self.id, pattern))?
                        .filter_map(|r| r.ok())
                        .collect();
                    if matches.is_empty() {
                        bail!("step '{}': glob '{}' matched no files", self.id, pattern);
                    }
                    matches.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));
                    matches
                }
                Source::Files(list) => {
                    let mut resolved: Vec<PathBuf> =
                        list.iter().map(PathBuf::from).collect();
                    resolved.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));
                    resolved
                }
            };

            // Verify all inputs exist before running any migration.
            for f in &files {
                if !f.exists() {
                    bail!("step '{}': file not found: {}", self.id, f.display());
                }
            }

            let cwd = self.cwd.as_deref();

            info!("[{}] applying {} migration(s)", self.id, files.len());
            for file in &files {
                let argv = render_argv(&self.command, file);
                info!("[{}] $ {}", self.id, argv.join(" "));
                let mut cmd = Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                if let Some(c) = cwd {
                    cmd.current_dir(c);
                }
                let status = cmd
                    .status()
                    .await
                    .with_context(|| format!("step '{}': failed to spawn {}", self.id, argv[0]))?;
                if !status.success() {
                    bail!(
                        "step '{}': migration '{}' failed with {}",
                        self.id,
                        file.display(),
                        status
                    );
                }
            }

            Ok(StepOutcome::default())
        })
    }
}

/// Substitute `{file}` (full path) and `{name}` (stem) in every argv element.
/// Placeholders are step-local; pipeline-wide `${var}` interpolation runs
/// before the step is built, so those references are already resolved by
/// the time we get here.
fn render_argv(template: &[String], file: &Path) -> Vec<String> {
    let file_str = file.to_string_lossy().to_string();
    let name = file
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    template
        .iter()
        .map(|s| s.replace("{file}", &file_str).replace("{name}", &name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(body: &str) -> StepConfig {
        let extra: toml::Table = toml::from_str(body).unwrap();
        StepConfig {
            id: "test".to_string(),
            kind: "db_migrate".to_string(),
            extra,
        }
    }

    #[test]
    fn render_substitutes_file() {
        let out = render_argv(
            &["psql".into(), "-f".into(), "{file}".into()],
            Path::new("/tmp/001.sql"),
        );
        assert_eq!(out, vec!["psql", "-f", "/tmp/001.sql"]);
    }

    #[test]
    fn render_substitutes_name() {
        let out = render_argv(
            &["migrate".into(), "--only={name}".into()],
            Path::new("/tmp/001_init.sql"),
        );
        assert_eq!(out, vec!["migrate", "--only=001_init"]);
    }

    #[test]
    fn render_substitutes_both_placeholders_in_one_token() {
        let out = render_argv(
            &["--log={name}.out".into(), "{file}".into()],
            Path::new("/x/007_users.sql"),
        );
        assert_eq!(out, vec!["--log=007_users.out", "/x/007_users.sql"]);
    }

    #[test]
    fn render_without_placeholders_passes_through() {
        let out = render_argv(&["echo".into(), "hi".into()], Path::new("ignored"));
        assert_eq!(out, vec!["echo", "hi"]);
    }

    #[test]
    fn from_config_rejects_both_glob_and_files() {
        let err = DbMigrate::from_config(&cfg_with(
            r#"
                glob = "*.sql"
                files = ["a.sql"]
                command = ["echo"]
            "#,
        ))
        .err()
        .expect("expected error");
        assert!(err.to_string().contains("mutually exclusive"), "err: {err}");
    }

    #[test]
    fn from_config_rejects_neither_glob_nor_files() {
        let err = DbMigrate::from_config(&cfg_with(r#"command = ["echo"]"#))
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("must be set"), "err: {err}");
    }

    #[test]
    fn from_config_rejects_empty_command() {
        let err = DbMigrate::from_config(&cfg_with(
            r#"
                files = ["a.sql"]
                command = []
            "#,
        ))
        .err()
        .expect("expected error");
        assert!(err.to_string().contains("empty"), "err: {err}");
    }
}
