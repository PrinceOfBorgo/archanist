//! [`Step`] trait, execution context, and the built-in step registry.
//!
//! Adding a new step kind = implement the step type with an inherent
//! `from_body(toml::Value) -> Result<Self>` factory in a submodule, then
//! register it in [`builtin_registry`] so `${var}` interpolation, the
//! pipeline runner, and `archanist step-kinds` all see it.

pub mod config_merge;
pub mod copy_files;
pub mod db_migrate;
pub mod docker_swap;
pub mod download;
pub mod http_health;
pub mod parse_text;
pub mod shell;

use crate::config::StepConfig;
use crate::interp::{self, Env};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Execution context handed to every step.
///
/// Since the step's own id and kind live in [`StepConfig`] (not on the
/// step itself), the pipeline threads them through here so `apply` /
/// `rollback` implementations can include them in error messages and
/// use them to locate per-step state on disk.
pub struct StepCtx {
    /// The step's `id` from its [`StepConfig`]. Used in log lines and
    /// error contexts.
    pub step_id: String,
    /// Name of the component this step belongs to. Used by steps that
    /// persist per-component state (`db_migrate`'s applied-stems log).
    pub component: String,
    /// Base directory for resolving relative paths and persisting per-step
    /// state (e.g. `<base_dir>/.archanist-migrations/<component>/<step_id>.toml`).
    /// Comes from [`crate::config::ArchanistConfig::base_dir`].
    pub base_dir: PathBuf,
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

/// Behavior contract for a single pipeline step.
///
/// Step kinds implement this and expose an inherent
/// `from_body(toml::Value) -> Result<Self>` constructor that the
/// [`StepRegistry`] calls after `${var}` interpolation. The step itself
/// doesn't know its own id or kind - the pipeline passes the id via
/// [`StepCtx::step_id`] when it calls `apply` / `rollback`.
pub trait Step: Send + Sync {
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

    /// Read-only probe: is the target state that this step would produce
    /// already in place? Consumed by `archanist check` to report which
    /// steps would actually do work if the update were run right now.
    ///
    /// The default assumes "no" - safer to over-report work than to
    /// silently skip a step that isn't actually idempotent.
    fn is_satisfied<'a>(&'a self, _ctx: &'a StepCtx) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move { Ok(false) })
    }
}

/// A factory that turns an already-interpolated step body into a boxed
/// step. Registered against a `type` string in a [`StepRegistry`].
pub type StepFactory = Arc<dyn Fn(toml::Value) -> Result<Box<dyn Step>> + Send + Sync>;

/// Registry mapping step `type` strings to their factory closures.
///
/// Built once at startup via [`builtin_registry`] and shared across every
/// pipeline (`Arc`-cloneable, cheap to hand out).
#[derive(Default, Clone)]
pub struct StepRegistry {
    inner: HashMap<String, StepFactory>,
}

impl StepRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a factory under the given step type name. A later call
    /// with the same name replaces the previous registration.
    pub fn register<F>(&mut self, kind: &str, factory: F)
    where
        F: Fn(toml::Value) -> Result<Box<dyn Step>> + Send + Sync + 'static,
    {
        self.inner.insert(kind.into(), Arc::new(factory));
    }

    /// Build the step for `cfg`, interpolating `${var}` references in
    /// its body against `env` first. Returns an error if the step type
    /// isn't registered, if interpolation fails, or if the factory
    /// rejects the body.
    pub fn build(&self, cfg: &StepConfig, env: &Env) -> Result<Box<dyn Step>> {
        let factory = self
            .inner
            .get(&cfg.kind)
            .with_context(|| format!("step '{}': unsupported type '{}'", cfg.id, cfg.kind))?;
        let raw_body = toml::Value::Table(cfg.body.clone());
        let body = interp::interpolate_toml(&raw_body, env)
            .with_context(|| format!("step '{}': failed to interpolate body", cfg.id))?;
        factory(body).with_context(|| format!("step '{}': failed to build", cfg.id))
    }

    /// Build the step for `cfg` without touching its body - used by the
    /// rollback path, which consults each step's payload snapshot
    /// rather than its (possibly stale) templated config.
    pub fn build_raw(&self, cfg: &StepConfig) -> Result<Box<dyn Step>> {
        let factory = self
            .inner
            .get(&cfg.kind)
            .with_context(|| format!("step '{}': unsupported type '{}'", cfg.id, cfg.kind))?;
        let body = toml::Value::Table(cfg.body.clone());
        factory(body).with_context(|| format!("step '{}': failed to build", cfg.id))
    }

    /// Names of every registered step kind, sorted for stable output.
    pub fn kinds(&self) -> Vec<&str> {
        let mut ks: Vec<&str> = self.inner.keys().map(String::as_str).collect();
        ks.sort_unstable();
        ks
    }
}

/// Build the registry populated with all built-in step kinds.
pub fn builtin_registry() -> StepRegistry {
    let mut r = StepRegistry::new();
    r.register("shell", |b| Ok(Box::new(shell::Shell::from_body(b)?)));
    r.register("download", |b| {
        Ok(Box::new(download::Download::from_body(b)?))
    });
    r.register("http_health", |b| {
        Ok(Box::new(http_health::HttpHealth::from_body(b)?))
    });
    r.register("copy_files", |b| {
        Ok(Box::new(copy_files::CopyFiles::from_body(b)?))
    });
    r.register("config_merge", |b| {
        Ok(Box::new(config_merge::ConfigMerge::from_body(b)?))
    });
    r.register("parse_text", |b| {
        Ok(Box::new(parse_text::ParseText::from_body(b)?))
    });
    r.register("db_migrate", |b| {
        Ok(Box::new(db_migrate::DbMigrateStep::from_body(b)?))
    });
    r.register("docker_swap", |b| {
        Ok(Box::new(docker_swap::DockerSwap::from_body(b)?))
    });
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_lists_every_kind() {
        let r = builtin_registry();
        let kinds = r.kinds();
        for expected in [
            "config_merge",
            "copy_files",
            "db_migrate",
            "docker_swap",
            "download",
            "http_health",
            "parse_text",
            "shell",
        ] {
            assert!(
                kinds.contains(&expected),
                "built-in kind {expected:?} missing from registry: {kinds:?}"
            );
        }
    }

    #[test]
    fn registry_rejects_unknown_kind() {
        let r = builtin_registry();
        let cfg = StepConfig {
            id: "x".into(),
            kind: "nope".into(),
            body: toml::Table::new(),
        };
        let err = r.build(&cfg, &Env::new()).err().expect("expected error");
        assert!(
            err.to_string().contains("unsupported type"),
            "expected 'unsupported type' error, got: {err}"
        );
    }
}
