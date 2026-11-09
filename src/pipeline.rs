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

use crate::config::{ArchanistConfig, ComponentConfig, StepConfig};
use crate::interp::{self, Env};
use crate::steps::recipe_sync::RecipeSync;
use crate::steps::{StepCtx, StepKind, StepOutcome, StepRegistry};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

pub struct Pipeline {
    component_name: String,
    is_self_update: bool,
    base_dir: PathBuf,
    components_dir: PathBuf,
    step_configs: Vec<StepConfig>,
    registry: Arc<StepRegistry>,
    recipe: Option<WatchedRecipe>,
}

/// The component's recipe file as it was when `step_configs` was loaded.
/// A step may rewrite it mid-run; the pipeline switches to the new version
/// only when a `recipe_sync` with `reload = true` asks for it.
struct WatchedRecipe {
    path: PathBuf,
    snapshot: Option<String>,
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
        step_configs: Vec<StepConfig>,
        is_self_update: bool,
        base_dir: PathBuf,
        components_dir: PathBuf,
        registry: Arc<StepRegistry>,
    ) -> Self {
        debug!(
            "built pipeline for '{}' with {} step(s) (self_update={})",
            name,
            step_configs.len(),
            is_self_update
        );
        Self {
            component_name: name.to_string(),
            is_self_update,
            base_dir,
            components_dir,
            step_configs,
            registry,
            recipe: None,
        }
    }

    /// Re-read the recipe at `path` after every completed step. When its
    /// content changes and a reload is requested (see
    /// [`Self::reloaded_recipe`]), the remaining steps and `[vars]` come
    /// from the new version; otherwise the change is only logged.
    pub fn watch_recipe(mut self, path: PathBuf) -> Self {
        let snapshot = std::fs::read_to_string(&path).ok();
        self.recipe = Some(WatchedRecipe { path, snapshot });
        self
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
        info!(
            "running pipeline for component '{}' ({} step(s))",
            self.component_name,
            self.step_configs.len()
        );
        let mut env = initial_env;
        let mut steps = self.step_configs.clone();
        let mut snapshot = self.recipe.as_ref().and_then(|r| r.snapshot.clone());
        let mut completed = HashSet::new();
        let mut i = 0;
        while i < steps.len() {
            let step_cfg = &steps[i];
            info!(
                "step {}/{}: [{}] {}",
                i + 1,
                steps.len(),
                step_cfg.kind,
                step_cfg.id
            );
            let step = self.registry.build(step_cfg, &env)?;
            let ctx = StepCtx {
                step_id: step_cfg.id.clone(),
                component: self.component_name.clone(),
                base_dir: self.base_dir.clone(),
                components_dir: self.components_dir.clone(),
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
            completed.insert(step_cfg.id.clone());
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
            if let Some((new, pos)) = self.reloaded_recipe(&mut snapshot, &steps[i], &outcome) {
                let done_id = steps[i].id.clone();
                steps.truncate(i + 1);
                let (skipped, rest): (Vec<_>, Vec<_>) = new.steps[pos + 1..]
                    .iter()
                    .cloned()
                    .partition(|s| completed.contains(&s.id));
                if !skipped.is_empty() {
                    let ids: Vec<_> = skipped.iter().map(|s| s.id.as_str()).collect();
                    info!(
                        "skipping step(s) already completed in this run: {}",
                        ids.join(", ")
                    );
                }
                steps.extend(rest);
                env = interp::expand_component_vars(&env, &new.vars);
                info!(
                    "recipe for '{}' reloaded after step '{}'; continuing with the updated recipe ({} step(s) left)",
                    self.component_name,
                    done_id,
                    steps.len() - i - 1
                );
            }
            i += 1;
        }
        info!(
            "pipeline for '{}' completed successfully",
            self.component_name
        );
        Ok(())
    }

    /// The new recipe to continue with, and the position of `done` in it,
    /// if the watched recipe changed on disk and a reload was requested:
    /// by `done` itself when it is a `recipe_sync`, otherwise by the new
    /// recipe declaring `done` as a `recipe_sync` with `reload = true` (so
    /// an older recipe that only copies the new one in still hands over).
    /// Any other change is logged and the run keeps its current recipe.
    fn reloaded_recipe(
        &self,
        snapshot: &mut Option<String>,
        done: &StepConfig,
        outcome: &StepOutcome,
    ) -> Option<(ComponentConfig, usize)> {
        let Some(recipe) = &self.recipe else {
            if outcome.reload_recipe {
                info!(
                    "step '{}' asked to reload the recipe for '{}', but this run doesn't reload recipes",
                    done.id, self.component_name
                );
            }
            return None;
        };
        let content = std::fs::read_to_string(&recipe.path).ok()?;
        if snapshot.as_deref() == Some(content.as_str()) {
            return None;
        }
        *snapshot = Some(content.clone());

        let keep = "the rest of this run keeps the recipe it started with";
        let new = match ArchanistConfig::parse_component(&content, &recipe.path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "recipe for '{}' changed on disk but can't be loaded; {}: {:#}",
                    self.component_name, keep, e
                );
                return None;
            }
        };
        let Some(pos) = new.steps.iter().position(|s| s.id == done.id) else {
            warn!(
                "recipe for '{}' changed on disk but no longer has step '{}'; {}",
                self.component_name, done.id, keep
            );
            return None;
        };

        let by_recipe_sync = done.kind == RecipeSync::KIND;
        let reload = if by_recipe_sync {
            outcome.reload_recipe
        } else {
            requests_reload(&new.steps[pos])
        };
        if reload {
            return Some((new, pos));
        }
        if by_recipe_sync {
            info!(
                "recipe for '{}' updated by step '{}' (reload = false); {}",
                self.component_name, done.id, keep
            );
        } else {
            warn!(
                "recipe for '{}' changed on disk after step '{}'; {} (use a `recipe_sync` step with `reload = true` to apply recipe changes mid-run)",
                self.component_name, done.id, keep
            );
        }
        None
    }
}

fn requests_reload(step: &StepConfig) -> bool {
    step.kind == RecipeSync::KIND
        && step.body.get("reload").and_then(toml::Value::as_bool) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steps::{BoxFuture, Step};
    use std::path::Path;
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<String>>>;

    /// Overwrites the recipe file with `content`, like a recipe sync.
    struct Rewrite {
        path: PathBuf,
        content: String,
        reload: bool,
    }

    impl Step for Rewrite {
        fn apply<'a>(&'a self, _ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
            Box::pin(async move {
                std::fs::write(&self.path, &self.content)?;
                Ok(StepOutcome {
                    reload_recipe: self.reload,
                    ..Default::default()
                })
            })
        }
    }

    /// Appends its interpolated `msg` to the shared log.
    struct Record {
        msg: String,
        log: Log,
    }

    impl Step for Record {
        fn apply<'a>(&'a self, _ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
            Box::pin(async move {
                self.log.lock().unwrap().push(self.msg.clone());
                Ok(StepOutcome::default())
            })
        }
    }

    /// `copy` stands in for a plain file copy, `recipe_sync` for the real
    /// step; both write `new_content` over the recipe.
    fn registry(recipe: &Path, new_content: &str, log: &Log) -> Arc<StepRegistry> {
        let mut reg = StepRegistry::new();
        for kind in ["copy", RecipeSync::KIND] {
            let (path, content) = (recipe.to_path_buf(), new_content.to_string());
            reg.register(kind, move |body| {
                Ok(Box::new(Rewrite {
                    path: path.clone(),
                    content: content.clone(),
                    reload: body.get("reload").and_then(|v| v.as_bool()) == Some(true),
                }) as Box<dyn Step>)
            });
        }
        let log = log.clone();
        reg.register("record", move |body| {
            let msg = body
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Ok(Box::new(Record {
                msg,
                log: log.clone(),
            }) as Box<dyn Step>)
        });
        Arc::new(reg)
    }

    /// An older recipe whose `sync` step just copies the new recipe in.
    const OLD: &str = r#"
schema = 1
[vars]
tag = "old"
[[steps]]
id = "prep"
type = "record"
msg = "prep"
[[steps]]
id = "sync"
type = "copy"
[[steps]]
id = "swap"
type = "record"
msg = "swap ${vars.tag}"
"#;

    /// Moves `sync` first and opts in to the reload.
    const NEW: &str = r#"
schema = 1
[vars]
tag = "new"
[[steps]]
id = "sync"
type = "recipe_sync"
reload = true
[[steps]]
id = "prep"
type = "record"
msg = "prep"
[[steps]]
id = "swap"
type = "record"
msg = "swap ${vars.tag}"
[[steps]]
id = "extra"
type = "record"
msg = "extra"
"#;

    const RELOADED: [&str; 3] = ["prep", "swap new", "extra"];
    const KEPT: [&str; 2] = ["prep", "swap old"];

    async fn run(name: &str, old: &str, new: &str, watch: bool) -> Vec<String> {
        let dir = std::env::temp_dir().join(format!("arch-pipe-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let recipe = dir.join("comp.toml");
        std::fs::write(&recipe, old).unwrap();

        let comp = ArchanistConfig::parse_component(old, &recipe).unwrap();
        let log: Log = Arc::default();
        let mut pipeline = Pipeline::build(
            "comp",
            comp.steps,
            false,
            dir.clone(),
            dir.clone(),
            registry(&recipe, new, &log),
        );
        if watch {
            pipeline = pipeline.watch_recipe(recipe.clone());
        }
        let env = interp::expand_component_vars(&Env::new(), &comp.vars);
        pipeline.run(env, |_, _| Ok(())).await.unwrap();

        std::fs::remove_dir_all(&dir).ok();
        log.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn new_recipe_opting_in_takes_over_and_skips_completed_steps() {
        assert_eq!(run("handover", OLD, NEW, true).await, RELOADED);
    }

    #[tokio::test]
    async fn change_without_opt_in_is_only_logged() {
        let plain = NEW.replace("reload = true", "");
        assert_eq!(run("plain", OLD, &plain, true).await, KEPT);
    }

    #[tokio::test]
    async fn running_recipe_sync_decides_the_reload() {
        let sync = |reload: &str| {
            OLD.replace(
                r#"type = "copy""#,
                &format!("type = \"recipe_sync\"\n{reload}"),
            )
        };
        let new_plain = NEW.replace("reload = true", "");
        assert_eq!(
            run("sync-on", &sync("reload = true"), &new_plain, true).await,
            RELOADED
        );
        assert_eq!(run("sync-off", &sync(""), NEW, true).await, KEPT);
    }

    #[tokio::test]
    async fn unwatched_pipeline_keeps_the_loaded_steps() {
        assert_eq!(run("nowatch", OLD, NEW, false).await, KEPT);
    }

    #[tokio::test]
    async fn recipe_without_the_completed_step_is_ignored() {
        let renamed = NEW.replace(r#"id = "sync""#, r#"id = "other""#);
        assert_eq!(run("missing", OLD, &renamed, true).await, KEPT);
    }

    #[tokio::test]
    async fn unparsable_recipe_is_ignored() {
        assert_eq!(run("broken", OLD, "not = [valid", true).await, KEPT);
    }
}
