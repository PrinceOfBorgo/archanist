pub mod config_merge;
pub mod copy_files;
pub mod download;
pub mod http_health;
pub mod shell;

use crate::config::StepConfig;
use anyhow::{Result, bail};
use std::future::Future;
use std::pin::Pin;

pub type ExecuteFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

pub trait Step: Send + Sync {
    fn from_config(cfg: &StepConfig) -> Result<Self>
    where
        Self: Sized;

    fn id(&self) -> &str;
    fn kind(&self) -> &str;
    fn describe(&self) -> String;
    fn execute(&self) -> ExecuteFuture<'_>;
}

pub fn build_step(cfg: &StepConfig) -> Result<Box<dyn Step>> {
    match cfg.kind.as_str() {
        "shell" => Ok(Box::new(shell::Shell::from_config(cfg)?)),
        "download" => Ok(Box::new(download::Download::from_config(cfg)?)),
        "http_health" => Ok(Box::new(http_health::HttpHealth::from_config(cfg)?)),
        "copy_files" => Ok(Box::new(copy_files::CopyFiles::from_config(cfg)?)),
        "config_merge" => Ok(Box::new(config_merge::ConfigMerge::from_config(cfg)?)),
        other => bail!("step '{}': unsupported type '{}'", cfg.id, other),
    }
}
