//! `db_migrate`: driver-agnostic forward-only migration runner.
//!
//! Migration files are discovered via `glob` **or** listed explicitly
//! in `files` (the two are mutually exclusive) and applied in
//! lexicographic order of file stem. Each file is passed to a
//! user-supplied `command` template with the placeholders `{file}`
//! (full path) and `{name}` (file stem) substituted per invocation.

use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::info;

pub struct DbMigrate {
    source: Source,
    command: Vec<String>,
    cwd: Option<String>,
}

enum Source {
    Glob(String),
    Files(Vec<String>),
}

#[derive(Deserialize)]
struct DbMigrateRaw {
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    files: Option<Vec<String>>,
    command: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

impl DbMigrate {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: DbMigrateRaw = body.try_into().context("invalid db_migrate config")?;

        if raw.command.is_empty() {
            bail!("`command` must not be empty");
        }

        let source = match (raw.glob, raw.files) {
            (Some(g), None) => Source::Glob(g),
            (None, Some(f)) => Source::Files(f),
            (Some(_), Some(_)) => bail!("`glob` and `files` are mutually exclusive"),
            (None, None) => bail!("one of `glob` or `files` must be set"),
        };

        Ok(Self {
            source,
            command: raw.command,
            cwd: raw.cwd,
        })
    }
}

impl Step for DbMigrate {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let files: Vec<PathBuf> = match &self.source {
                Source::Glob(pattern) => {
                    info!("[{}] expanding glob: {}", id, pattern);
                    let mut matches: Vec<PathBuf> = glob::glob(pattern)
                        .with_context(|| format!("step '{}': invalid glob '{}'", id, pattern))?
                        .filter_map(|r| r.ok())
                        .collect();
                    if matches.is_empty() {
                        bail!("step '{}': glob '{}' matched no files", id, pattern);
                    }
                    matches.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));
                    matches
                }
                Source::Files(list) => {
                    let mut resolved: Vec<PathBuf> = list.iter().map(PathBuf::from).collect();
                    resolved.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));
                    resolved
                }
            };

            // Verify all inputs exist before running any migration.
            for f in &files {
                if !f.exists() {
                    bail!("step '{}': file not found: {}", id, f.display());
                }
            }

            let cwd = self.cwd.as_deref();

            info!("[{}] applying {} migration(s)", id, files.len());
            for file in &files {
                let argv = render_argv(&self.command, file);
                info!("[{}] $ {}", id, argv.join(" "));
                let mut cmd = Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                if let Some(c) = cwd {
                    cmd.current_dir(c);
                }
                let status = cmd
                    .status()
                    .await
                    .with_context(|| format!("step '{}': failed to spawn {}", id, argv[0]))?;
                if !status.success() {
                    bail!(
                        "step '{}': migration '{}' failed with {}",
                        id,
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

    fn body(text: &str) -> toml::Value {
        toml::Value::Table(toml::from_str(text).unwrap())
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
    fn from_body_rejects_both_glob_and_files() {
        let err = DbMigrate::from_body(body(
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
    fn from_body_rejects_neither_glob_nor_files() {
        let err = DbMigrate::from_body(body(r#"command = ["echo"]"#))
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("must be set"), "err: {err}");
    }

    #[test]
    fn from_body_rejects_empty_command() {
        let err = DbMigrate::from_body(body(
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
