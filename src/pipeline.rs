use crate::config::ComponentConfig;
use crate::interp::Env;
use crate::steps::{self, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::{debug, info};

pub struct Pipeline {
    component_name: String,
    steps: Vec<Box<dyn Step>>,
}

impl Pipeline {
    pub fn build(name: &str, cfg: &ComponentConfig) -> Result<Self> {
        let steps = cfg
            .steps
            .iter()
            .map(steps::build_step)
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("failed to build pipeline for '{}'", name))?;
        debug!("built pipeline for '{}' with {} step(s)", name, steps.len());
        Ok(Self {
            component_name: name.to_string(),
            steps,
        })
    }

    pub fn steps(&self) -> &[Box<dyn Step>] {
        &self.steps
    }

    /// Run the pipeline. `on_step_complete` is called with the step and its
    /// outcome after each successful step, allowing callers to persist state
    /// (per-step `applied_steps` for rollback) as the pipeline progresses.
    pub async fn run<F>(&self, mut on_step_complete: F) -> Result<()>
    where
        F: FnMut(&dyn Step, &StepOutcome) -> Result<()>,
    {
        let total = self.steps.len();
        info!(
            "running pipeline for component '{}' ({} step(s))",
            self.component_name, total
        );
        let mut env = Env::new();
        for (i, step) in self.steps.iter().enumerate() {
            info!("step {}/{}: [{}] {}", i + 1, total, step.kind(), step.id());
            let ctx = StepCtx {
                vars: Arc::new(env.clone()),
            };
            let outcome = step
                .apply(&ctx)
                .await
                .with_context(|| format!("step '{}' failed", step.id()))?;
            on_step_complete(&**step, &outcome).with_context(|| {
                format!("failed to persist state after step '{}'", step.id())
            })?;
            for (k, v) in &outcome.exported_vars {
                env.insert(k.clone(), v.clone());
            }
            if outcome.exit_after {
                info!(
                    "step '{}' requested exit_after; leaving pipeline for '{}'",
                    step.id(),
                    self.component_name
                );
                return Ok(());
            }
        }
        info!(
            "pipeline for '{}' completed successfully",
            self.component_name
        );
        Ok(())
    }
}
