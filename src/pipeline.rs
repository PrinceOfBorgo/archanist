use crate::config::{ComponentConfig, StepConfig};
use crate::interp::Env;
use crate::steps::{self, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result};
use tracing::{debug, info};

pub struct Pipeline {
    component_name: String,
    is_self_update: bool,
    step_configs: Vec<StepConfig>,
}

impl Pipeline {
    pub fn build(name: &str, cfg: &ComponentConfig, is_self_update: bool) -> Self {
        let step_configs = cfg.steps.clone();
        debug!(
            "built pipeline for '{}' with {} step(s) (self_update={})",
            name,
            step_configs.len(),
            is_self_update
        );
        Self {
            component_name: name.to_string(),
            is_self_update,
            step_configs,
        }
    }

    /// Run the pipeline starting from `initial_env`. Each step's body is
    /// interpolated against the current environment right before build,
    /// so later steps see variables exported by earlier ones.
    /// `on_step_complete` is called with the step and its outcome after
    /// each successful step, allowing callers to persist state (per-step
    /// `applied_steps` for rollback) as the pipeline progresses.
    pub async fn run<F>(&self, initial_env: Env, mut on_step_complete: F) -> Result<()>
    where
        F: FnMut(&dyn Step, &StepOutcome) -> Result<()>,
    {
        let total = self.step_configs.len();
        info!(
            "running pipeline for component '{}' ({} step(s))",
            self.component_name, total
        );
        let mut env = initial_env;
        for (i, step_cfg) in self.step_configs.iter().enumerate() {
            let step = steps::build_step_interpolated(step_cfg, &env)
                .with_context(|| format!("failed to build step '{}'", step_cfg.id))?;
            info!("step {}/{}: [{}] {}", i + 1, total, step.kind(), step.id());
            let ctx = StepCtx {
                is_self_update: self.is_self_update,
            };
            let outcome = step
                .apply(&ctx)
                .await
                .with_context(|| format!("step '{}' failed", step.id()))?;
            on_step_complete(&*step, &outcome).with_context(|| {
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
