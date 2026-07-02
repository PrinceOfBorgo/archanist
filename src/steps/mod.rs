pub mod config_merge;
pub mod copy_files;
pub mod db_migrate;
pub mod download;
pub mod http_health;
pub mod parse_text;
pub mod shell;

use crate::config::StepConfig;
use crate::interp::Env;
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
    fn execute<'a>(&'a self, env: &'a mut Env) -> ExecuteFuture<'a>;
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
        other => bail!("step '{}': unsupported type '{}'", cfg.id, other),
    }
}
