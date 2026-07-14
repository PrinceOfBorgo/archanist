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
}

/// Result of a step's `apply` call.
#[derive(Debug, Default)]
pub struct StepOutcome {
    /// If true, the pipeline should stop after this step and return Ok.
    /// Used by `docker_swap` self-update so the process exits cleanly and
    /// the new container image can take over.
    pub exit_after: bool,
    /// Variables to publish for subsequent steps in the same pipeline.
    pub exported_vars: Env,
}

pub trait Step: Send + Sync {
    fn from_config(cfg: &StepConfig) -> Result<Self>
    where
        Self: Sized;

    fn id(&self) -> &str;
    fn kind(&self) -> &str;
    fn describe(&self) -> String;
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>>;
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
