//! High-level orchestration: drives the [`Step`](crate::steps::Step)
//! pipeline for one component.
//!
//! Each step's body is interpolated against the current env immediately
//! before the step is built (via [`StepRegistry::build`]), so vars
//! exported by earlier steps (via [`StepOutcome::exported_vars`]) are
//! visible to later ones. Step lifecycle events are surfaced through
//! the caller-supplied `on_step_event` callback so the outer command
//! can update the persisted [`crate::state::UpdateAttempt`] as the run
//! progresses.

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

/// Lifecycle events surfaced to the [`Pipeline::run`] callback.
///
/// The pipeline never fires [`StepEvent::Completed`] for a failed step;
/// failure propagates out of `run` and the caller reconstructs which
/// step was in flight from its own [`crate::state::UpdateAttempt`]
/// records.
pub enum StepEvent<'a> {
    /// About to invoke `apply` for this step.
    Started,
    /// `apply` returned successfully with this outcome.
    Completed(&'a StepOutcome),
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
    ///
    /// `on_step_event` is invoked at [`StepEvent::Started`] and, on
    /// success, [`StepEvent::Completed`]. It is not called for a step
    /// whose `apply` errors - that error bubbles out of `run` and the
    /// caller uses its own state to identify the failing step.
    pub async fn run<F>(&self, initial_env: Env, mut on_step_event: F) -> Result<()>
    where
        F: FnMut(&StepConfig, StepEvent<'_>) -> Result<()>,
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
            on_step_event(step_cfg, StepEvent::Started).with_context(|| {
                format!("failed to persist state before step '{}'", step_cfg.id)
            })?;
            let outcome = step
                .apply(&ctx)
                .await
                .with_context(|| format!("step '{}' failed", step_cfg.id))?;
            on_step_event(step_cfg, StepEvent::Completed(&outcome))
                .with_context(|| format!("failed to persist state after step '{}'", step_cfg.id))?;
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
