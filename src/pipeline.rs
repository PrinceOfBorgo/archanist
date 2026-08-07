//! High-level orchestration: drives the [`Step`](crate::steps::Step)
//! pipeline for one component.
//!
//! Each step's body is interpolated against the current env immediately
//! before the step is built (via [`StepRegistry::build`]), so vars
//! exported by earlier steps (via [`StepOutcome::exported_vars`]) are
//! visible to later ones. State is persisted per-step through the
//! `on_step_complete` callback so a failed run can be resumed or
//! rolled back.

use crate::config::{ComponentConfig, StepConfig};
use crate::interp::Env;
use crate::steps::{StepCtx, StepOutcome, StepRegistry};
use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::{debug, info};

pub struct Pipeline {
    component_name: String,
    is_self_update: bool,
    step_configs: Vec<StepConfig>,
    registry: Arc<StepRegistry>,
}

impl Pipeline {
    pub fn build(
        name: &str,
        cfg: &ComponentConfig,
        is_self_update: bool,
        registry: Arc<StepRegistry>,
    ) -> Self {
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
            registry,
        }
    }

    /// Run the pipeline starting from `initial_env`. Each step's body is
    /// interpolated against the current environment right before build,
    /// so later steps see variables exported by earlier ones.
    /// `on_step_complete` receives the completed step's config and its
    /// outcome, allowing callers to persist state (per-step
    /// `applied_steps` for rollback) as the pipeline progresses.
    pub async fn run<F>(&self, initial_env: Env, mut on_step_complete: F) -> Result<()>
    where
        F: FnMut(&StepConfig, &StepOutcome) -> Result<()>,
    {
        let total = self.step_configs.len();
        info!(
            "running pipeline for component '{}' ({} step(s))",
            self.component_name, total
        );
        let mut env = initial_env;
        for (i, step_cfg) in self.step_configs.iter().enumerate() {
            info!(
                "step {}/{}: [{}] {}",
                i + 1,
                total,
                step_cfg.kind,
                step_cfg.id
            );
            let step = self.registry.build(step_cfg, &env)?;
            let ctx = StepCtx {
                step_id: step_cfg.id.clone(),
                is_self_update: self.is_self_update,
            };
            let outcome = step
                .apply(&ctx)
                .await
                .with_context(|| format!("step '{}' failed", step_cfg.id))?;
            on_step_complete(step_cfg, &outcome).with_context(|| {
                format!("failed to persist state after step '{}'", step_cfg.id)
            })?;
            for (k, v) in &outcome.exported_vars {
                env.insert(k.clone(), v.clone());
            }
            if outcome.exit_after {
                info!(
                    "step '{}' requested exit_after; leaving pipeline for '{}'",
                    step_cfg.id, self.component_name
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
