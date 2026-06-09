pub mod shell;

use crate::config::StepConfig;
use anyhow::{Result, bail};

pub trait Step {
    fn id(&self) -> &str;
    fn kind(&self) -> &str;
    fn describe(&self) -> String;
    fn execute(&self) -> Result<()>;
}

pub fn build_step(cfg: &StepConfig) -> Result<Box<dyn Step>> {
    match cfg.kind.as_str() {
        "shell" => Ok(Box::new(shell::Shell::from_config(cfg)?)),
        other => bail!("step '{}': unsupported type '{}'", cfg.id, other),
    }
}
