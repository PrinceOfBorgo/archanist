pub mod config_merge;
pub mod copy_files;
pub mod db_migrate;
pub mod docker_swap;
pub mod download;
pub mod http_health;
pub mod parse_text;
pub mod shell;

use crate::config::StepConfig;
use crate::interp::Env;
use anyhow::{Result, bail};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Execution context handed to every step.
pub struct StepCtx {
    pub vars: Arc<Env>,
    /// True when the pipeline is updating the archanist itself. Consumed by
    /// `docker_swap` to force `exit_after` even when the step's own `self`
    /// field is unset (declarative sugar for "this component is me").
    pub is_self_update: bool,
}

/// Result of a step's `apply` call.
#[derive(Debug)]
pub struct StepOutcome {
    /// If true, the pipeline should stop after this step and return Ok.
    /// Used by `docker_swap` self-update so the process exits cleanly and
    /// the new container image can take over.
    pub exit_after: bool,
    /// Variables to publish for subsequent steps in the same pipeline.
    pub exported_vars: Env,
    /// Opaque payload persisted per-step in state. Consumed by `rollback`
    /// to know what to undo.
    pub payload: toml::Value,
}

impl Default for StepOutcome {
    fn default() -> Self {
        Self {
            exit_after: false,
            exported_vars: Env::new(),
            payload: toml::Value::Table(toml::Table::new()),
        }
    }
}

pub trait Step: Send + Sync {
    fn from_config(cfg: &StepConfig) -> Result<Self>
    where
        Self: Sized;

    fn id(&self) -> &str;
    fn kind(&self) -> &str;
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>>;

    /// Undo a previously-applied step. Receives the payload recorded by
    /// `apply`. Default implementation is a no-op for steps that don't
    /// need rollback semantics.
    fn rollback<'a>(
        &'a self,
        _ctx: &'a StepCtx,
        _payload: &'a toml::Value,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Ok(()) })
    }
}

pub fn build_step(cfg: &StepConfig) -> Result<Box<dyn Step>> {
    match cfg.kind.as_str() {
        "shell" => Ok(Box::new(shell::Shell::from_config(cfg)?)),
        "download" => Ok(Box::new(download::Download::from_config(cfg)?)),
        "http_health" => Ok(Box::new(http_health::HttpHealth::from_config(cfg)?)),
        "copy_files" => Ok(Box::new(copy_files::CopyFiles::from_config(cfg)?)),
        "config_merge" => Ok(Box::new(config_merge::ConfigMerge::from_config(cfg)?)),
        "parse_text" => Ok(Box::new(parse_text::ParseText::from_config(cfg)?)),
        "db_migrate" => Ok(Box::new(db_migrate::DbMigrate::from_config(cfg)?)),
        "docker_swap" => Ok(Box::new(docker_swap::DockerSwap::from_config(cfg)?)),
        other => bail!("step '{}': unsupported type '{}'", cfg.id, other),
    }
}

/// Names of all built-in step kinds recognized by [`build_step`].
pub const KINDS: &[&str] = &[
    "shell",
    "download",
    "http_health",
    "copy_files",
    "config_merge",
    "parse_text",
    "db_migrate",
    "docker_swap",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_all_registered() {
        for &kind in KINDS {
            let cfg = StepConfig {
                id: "x".into(),
                kind: kind.into(),
                extra: toml::Table::new(),
            };
            // We don't care whether build_step succeeds (most kinds require
            // fields we haven't provided); we only care that it doesn't fail
            // with an "unsupported type" error, which would indicate `KINDS`
            // drifted out of sync with the match arms.
            if let Some(e) = build_step(&cfg).err() {
                assert!(
                    !e.to_string().contains("unsupported type"),
                    "KIND {kind:?} listed but not registered in build_step: {e}"
                );
            }
        }
    }
}
