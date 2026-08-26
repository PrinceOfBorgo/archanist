//! `db_migrate`: driver-agnostic forward-only migration runner.
//!
//! Source modes (mutually exclusive):
//!
//! * **`glob`** - discover migration files by glob pattern (lexicographic
//!   order by file stem).
//! * **`files`** - explicit list of migration file paths. Accepts either
//!   a TOML array of strings, or a single string split on `split`
//!   (newline by default).
//!
//! Non-absolute paths from either source are resolved against `base` (or,
//! if unset, the archanist base dir).
//!
//! Each migration is invoked via the `command` argv with the step-local
//! placeholders `{file}` (full path) and `{name}` (file stem) substituted.
//! Applied stems are persisted to
//! `<base_dir>/.archanist-migrations/<component>/<step_id>.toml` so retries
//! and re-applies skip work that already succeeded. Rollback iterates the
//! stems applied by the current attempt in reverse and runs
//! `rollback_command` (a no-op when it's absent - the step is
//! forward-only).

use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{info, warn};

/// Accepts either a pre-split TOML array of paths or a delimited string.
///
/// The [`DbMigrateStep::split`] field controls how [`FilesInput::Delimited`]
/// is broken apart. Whitespace is trimmed and empty entries are dropped.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum FilesInput {
    /// Explicit list of paths.
    List(Vec<String>),
    /// Single string split on `split` into paths.
    Delimited(String),
}

#[derive(Debug, Deserialize)]
pub struct DbMigrateStep {
    /// Glob (resolved against `base`) selecting migration files. Mutually
    /// exclusive with `files`.
    #[serde(default)]
    pub glob: Option<String>,
    /// Explicit file list. Mutually exclusive with `glob`.
    #[serde(default)]
    pub files: Option<FilesInput>,
    /// Separator used to split `files` when it's a string. Default `"\n"`.
    #[serde(default = "default_split")]
    pub split: String,
    /// Optional base directory prepended to non-absolute paths from
    /// `files` / `glob`. Defaults to [`StepCtx::base_dir`].
    #[serde(default)]
    pub base: Option<String>,
    /// argv template. `{file}` is replaced with the migration file path,
    /// `{name}` with the file stem.
    pub command: Vec<String>,
    /// Optional argv template used by [`Step::rollback`] to revert a
    /// single migration by stem. When unset the step is forward-only
    /// and rollback is a warned no-op.
    #[serde(default)]
    pub rollback_command: Option<Vec<String>>,
    /// Optional working directory for `command` / `rollback_command`.
    #[serde(default)]
    pub cwd: Option<String>,
}

fn default_split() -> String {
    "\n".to_string()
}

/// Persisted per-component/per-step applied-migrations log, plus the
/// per-attempt payload written to [`StepOutcome::payload`].
#[derive(Debug, Default, Serialize, Deserialize)]
struct MigratePayload {
    /// Stems applied. When persisted to the on-disk log this is the
    /// cumulative set across all attempts. When emitted as the step's
    /// rollback payload it is only the stems applied by this attempt.
    #[serde(default)]
    applied: Vec<String>,
}

impl DbMigrateStep {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let step: DbMigrateStep = body.try_into().context("invalid db_migrate config")?;
        if step.command.is_empty() {
            bail!("`command` must not be empty");
        }
        match (&step.glob, &step.files) {
            (None, None) => bail!("one of `glob` or `files` must be set"),
            (Some(_), Some(_)) => bail!("`glob` and `files` are mutually exclusive"),
            _ => {}
        }
        Ok(step)
    }

    fn resolved_base(&self, ctx: &StepCtx) -> PathBuf {
        match &self.base {
            Some(b) => PathBuf::from(b),
            None => ctx.base_dir.clone(),
        }
    }

    fn payload_path(&self, ctx: &StepCtx) -> PathBuf {
        ctx.base_dir
            .join(".archanist-migrations")
            .join(&ctx.component)
            .join(format!("{}.toml", ctx.step_id))
    }
}

fn resolve(base: &Path, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() { p } else { base.join(p) }
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

async fn run(argv: &[String], cwd: Option<&str>, id: &str) -> Result<()> {
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
        bail!("step '{}': command exited with {}: {:?}", id, status, argv);
    }
    Ok(())
}

fn load_applied(path: &Path) -> HashSet<String> {
    if !path.exists() {
        return HashSet::new();
    }
    match std::fs::read_to_string(path) {
        Ok(contents) => match toml::from_str::<MigratePayload>(&contents) {
            Ok(p) => p.applied.into_iter().collect(),
            Err(_) => HashSet::new(),
        },
        Err(_) => HashSet::new(),
    }
}

fn save_applied(path: &Path, applied: &HashSet<String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create migrations log dir {}", parent.display()))?;
    }
    let mut sorted: Vec<String> = applied.iter().cloned().collect();
    sorted.sort();
    let content = toml::to_string_pretty(&MigratePayload { applied: sorted })
        .context("failed to serialize migrations log")?;
    std::fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

impl Step for DbMigrateStep {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let base = self.resolved_base(ctx);

            // Resolve candidate files from the configured source.
            let mut files: Vec<PathBuf> = if let Some(pattern) = &self.glob {
                let full = if PathBuf::from(pattern).is_absolute() {
                    pattern.clone()
                } else {
                    base.join(pattern).to_string_lossy().to_string()
                };
                info!("[{}] expanding glob: {}", id, full);
                let matches: Vec<PathBuf> = glob::glob(&full)
                    .with_context(|| format!("step '{}': invalid glob '{}'", id, full))?
                    .filter_map(|r| r.ok())
                    .collect();
                if matches.is_empty() {
                    bail!("step '{}': glob '{}' matched no files", id, full);
                }
                matches
            } else if let Some(input) = &self.files {
                let raw_list: Vec<String> = match input {
                    FilesInput::List(v) => v.clone(),
                    FilesInput::Delimited(s) => s
                        .split(self.split.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                };
                raw_list.iter().map(|s| resolve(&base, s)).collect()
            } else {
                unreachable!("validated in from_body");
            };

            // Verify all inputs exist before running any migration.
            for f in &files {
                if !f.exists() {
                    bail!("step '{}': file not found: {}", id, f.display());
                }
            }
            files.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));

            let payload_path = self.payload_path(ctx);
            let mut applied = load_applied(&payload_path);
            let cwd = self.cwd.as_deref();

            let mut applied_this_attempt: Vec<String> = Vec::new();
            info!("[{}] evaluating {} migration(s)", id, files.len());
            for file in &files {
                let stem = file
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if applied.contains(&stem) {
                    info!("[{}] skipping already-applied '{}'", id, stem);
                    continue;
                }
                let argv = render_argv(&self.command, file);
                run(&argv, cwd, id).await?;
                applied.insert(stem.clone());
                applied_this_attempt.push(stem);
                // Persist after every successful migration so a crash
                // mid-pipeline doesn't cause the survivor(s) to re-run.
                save_applied(&payload_path, &applied)?;
            }

            let attempt_payload = MigratePayload {
                applied: applied_this_attempt,
            };
            Ok(StepOutcome {
                payload: toml::Value::try_from(&attempt_payload)
                    .context("failed to serialize db_migrate payload")?,
                ..Default::default()
            })
        })
    }

    fn rollback<'a>(
        &'a self,
        ctx: &'a StepCtx,
        payload: &'a toml::Value,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let Some(template) = &self.rollback_command else {
                warn!(
                    "step '{}': no rollback_command set; leaving migrations applied",
                    id
                );
                return Ok(());
            };
            let pl: MigratePayload = payload.clone().try_into().unwrap_or_default();
            if pl.applied.is_empty() {
                return Ok(());
            }
            let payload_path = self.payload_path(ctx);
            let mut applied = load_applied(&payload_path);
            let cwd = self.cwd.as_deref();

            info!("[{}] rolling back {} migration(s)", id, pl.applied.len());
            for stem in pl.applied.iter().rev() {
                let argv = render_argv(template, Path::new(stem));
                run(&argv, cwd, id).await?;
                applied.remove(stem);
                save_applied(&payload_path, &applied)?;
            }
            Ok(())
        })
    }

    fn has_rollback(&self) -> bool {
        self.rollback_command.is_some()
    }
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
        let err = DbMigrateStep::from_body(body(
            r#"
                glob = "*.sql"
                files = ["a.sql"]
                command = ["echo"]
            "#,
        ))
        .expect_err("expected error");
        assert!(err.to_string().contains("mutually exclusive"), "err: {err}");
    }

    #[test]
    fn from_body_rejects_neither_glob_nor_files() {
        let err =
            DbMigrateStep::from_body(body(r#"command = ["echo"]"#)).expect_err("expected error");
        assert!(err.to_string().contains("must be set"), "err: {err}");
    }

    #[test]
    fn from_body_rejects_empty_command() {
        let err = DbMigrateStep::from_body(body(
            r#"
                files = ["a.sql"]
                command = []
            "#,
        ))
        .expect_err("expected error");
        assert!(err.to_string().contains("empty"), "err: {err}");
    }

    #[test]
    fn from_body_accepts_delimited_files_string() {
        let step = DbMigrateStep::from_body(body(
            r#"
                files = "001_init.sql\n002_users.sql\n"
                command = ["echo"]
            "#,
        ))
        .unwrap();
        assert!(matches!(step.files, Some(FilesInput::Delimited(_))));
    }

    #[test]
    fn from_body_accepts_files_list() {
        let step = DbMigrateStep::from_body(body(
            r#"
                files = ["001.sql", "002.sql"]
                command = ["echo"]
            "#,
        ))
        .unwrap();
        assert!(matches!(step.files, Some(FilesInput::List(_))));
    }

    #[test]
    fn from_body_defaults_split_to_newline() {
        let step = DbMigrateStep::from_body(body(
            r#"
                files = ["a.sql"]
                command = ["echo"]
            "#,
        ))
        .unwrap();
        assert_eq!(step.split, "\n");
    }

    #[test]
    fn from_body_accepts_rollback_command() {
        let step = DbMigrateStep::from_body(body(
            r#"
                files = ["a.sql"]
                command = ["migrate", "up", "{name}"]
                rollback_command = ["migrate", "down", "{name}"]
            "#,
        ))
        .unwrap();
        assert_eq!(
            step.rollback_command.as_deref(),
            Some(&["migrate".to_string(), "down".to_string(), "{name}".into()][..])
        );
    }

    #[test]
    fn load_save_roundtrip() {
        let dir = tempdir();
        let path = dir.join("log.toml");
        let mut applied = HashSet::new();
        applied.insert("001".to_string());
        applied.insert("002".to_string());
        save_applied(&path, &applied).unwrap();
        let back = load_applied(&path);
        assert_eq!(back, applied);
    }

    #[test]
    fn load_missing_file_returns_empty_set() {
        let dir = tempdir();
        let missing = dir.join("nope.toml");
        let back = load_applied(&missing);
        assert!(back.is_empty());
    }

    fn tempdir() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("archanist-db_migrate-test-{}", std::process::id()));
        let dir = base.join(format!(
            "case-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
