//! `archanist sandbox`: rehearse a component's update against throwaway
//! infrastructure described by the recipe's optional `[sandbox]` section.
//!
//! Everything the sandbox writes in-process lives under
//! `<base_dir>/sandbox/<component>/` (its own `state.toml`, migration logs,
//! backups), so the real state is never touched. Peer containers come from
//! `[[sandbox.services]]`, the pre-existing install the update starts from is
//! laid down once by `[[sandbox.setup]]`, and `[sandbox.vars]`,
//! `[sandbox.env]`, `[sandbox.override.<step>]` and `skip_steps` redirect the
//! real `[[steps]]` pipeline at all of that.

use crate::config::{ArchanistConfig, ComponentConfig, SandboxConfig, SandboxService};
use crate::docker::{DockerClient, ServiceSpec};
use crate::interp::{self, Env};
use crate::pipeline::Pipeline;
use crate::state::ArchanistState;
use crate::steps::StepRegistry;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub async fn run(
    cfg: &ArchanistConfig,
    component: &str,
    registry: &Arc<StepRegistry>,
) -> Result<()> {
    let (comp, sb) = lookup(cfg, component)?;
    let dir = sandbox_dir(cfg, component);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let env = setup_env(component, comp, sb, &dir);

    let docker = DockerClient::connect()?;
    if let Some(net) = &sb.network {
        println!("[sandbox] ensuring network {net}");
        docker.ensure_network(net).await?;
    }
    for svc in &sb.services {
        start_service(&docker, svc, sb.network.as_deref(), &env).await?;
    }

    let sb_cfg = sandbox_config(cfg, component, comp, sb, &dir);
    let state_path = sb_cfg.state_path();
    let mut state = ArchanistState::load(&state_path)?;

    if state.components.contains_key(component) {
        println!("[sandbox] {component}: baseline already in place, skipping setup");
    } else {
        if !sb.setup.is_empty() {
            println!(
                "[sandbox] {component}: laying down baseline {}",
                sb.baseline_version.as_deref().unwrap_or("(fresh install)")
            );
            Pipeline::build(
                component,
                sb.setup.clone(),
                false,
                dir.clone(),
                dir.join("components"),
                registry.clone(),
            )
            .run(env.clone(), |_, _| Ok(()))
            .await
            .context("sandbox setup failed")?;
        }
        state
            .components
            .entry(component.to_string())
            .or_default()
            .current_version = sb.baseline_version.clone();
        state.save(&state_path)?;
    }

    // A failed rehearsal shouldn't stop the next one from retrying the same version.
    if let Some(entry) = state.components.get_mut(component) {
        entry.blocklist.clear();
    }

    println!("[sandbox] {component}: running update");
    // No mid-run reload: the reloaded recipe would drop the sandbox overrides.
    let result = crate::run_update(
        &sb_cfg,
        &mut state,
        Some(component),
        &state_path,
        registry,
        &sandbox_env(sb, &dir),
        false,
    )
    .await;
    state.save(&state_path)?;

    println!();
    println!("[sandbox] ===== {component} summary =====");
    for name in container_names(sb, &env)? {
        match docker.container_image(&name).await {
            Ok(Some(image)) => println!("  container {name}: {image}"),
            _ => println!("  container {name}: (not present)"),
        }
    }
    if let Some(entry) = state.components.get(component) {
        println!(
            "  current version: {}",
            entry.current_version.as_deref().unwrap_or("(none)")
        );
    }
    println!("  sandbox dir: {}", dir.display());

    result
}

pub async fn rollback(
    cfg: &ArchanistConfig,
    component: &str,
    registry: &Arc<StepRegistry>,
) -> Result<()> {
    let (comp, sb) = lookup(cfg, component)?;
    let dir = sandbox_dir(cfg, component);
    let sb_cfg = sandbox_config(cfg, component, comp, sb, &dir);
    let state_path = sb_cfg.state_path();
    let mut state = ArchanistState::load(&state_path)?;
    let result = crate::run_rollback(
        &sb_cfg,
        &mut state,
        component,
        registry,
        &sandbox_env(sb, &dir),
    )
    .await;
    state.save(&state_path)?;
    result
}

pub async fn reset(cfg: &ArchanistConfig, component: &str) -> Result<()> {
    let (comp, sb) = lookup(cfg, component)?;
    let dir = sandbox_dir(cfg, component);
    let env = setup_env(component, comp, sb, &dir);

    let docker = DockerClient::connect()?;
    for name in container_names(sb, &env)? {
        docker.remove_container(&name).await?;
        println!("[sandbox] removed container {name}");
    }
    if let Some(net) = &sb.network {
        docker.remove_network(net).await?;
        println!("[sandbox] removed network {net}");
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("failed to remove {}", dir.display()))?;
        println!("[sandbox] removed {}", dir.display());
    }
    println!("[sandbox] {component}: clean");
    Ok(())
}

fn lookup<'a>(
    cfg: &'a ArchanistConfig,
    component: &str,
) -> Result<(&'a ComponentConfig, &'a SandboxConfig)> {
    let comp = cfg
        .components
        .get(component)
        .with_context(|| format!("unknown component '{component}'"))?;
    let sb = comp
        .sandbox
        .as_ref()
        .with_context(|| format!("component '{component}' has no [sandbox] section"))?;
    Ok((comp, sb))
}

fn sandbox_dir(cfg: &ArchanistConfig, component: &str) -> PathBuf {
    cfg.base_dir.join("sandbox").join(component)
}

/// Built-in `sandbox.*` vars plus the `[sandbox.env]` shadows of `${env.NAME}`.
fn sandbox_env(sb: &SandboxConfig, dir: &Path) -> Env {
    let mut env = Env::new();
    env.insert("sandbox.dir".into(), dir.to_string_lossy().into_owned());
    env.insert(
        "sandbox.network".into(),
        sb.network.clone().unwrap_or_default(),
    );
    env.insert(
        "sandbox.baseline_version".into(),
        sb.baseline_version.clone().unwrap_or_default(),
    );
    for (k, v) in &sb.env {
        env.insert(format!("env.{k}"), v.clone());
    }
    env
}

fn merged_vars(comp: &ComponentConfig, sb: &SandboxConfig) -> HashMap<String, String> {
    let mut vars = comp.vars.clone();
    vars.extend(sb.vars.clone());
    vars
}

/// Env used for services, setup steps and reset: `${version}` is the baseline.
fn setup_env(component: &str, comp: &ComponentConfig, sb: &SandboxConfig, dir: &Path) -> Env {
    let mut base = sandbox_env(sb, dir);
    base.insert(
        "version".into(),
        sb.baseline_version.clone().unwrap_or_default(),
    );
    base.insert("current_version".into(), "(none)".into());
    base.insert("component".into(), component.to_string());
    interp::expand_component_vars(&base, &merged_vars(comp, sb))
}

/// A config that only knows the sandboxed component and keeps its state
/// and per-step artifacts under the sandbox dir.
fn sandbox_config(
    cfg: &ArchanistConfig,
    component: &str,
    comp: &ComponentConfig,
    sb: &SandboxConfig,
    dir: &Path,
) -> ArchanistConfig {
    let steps = comp
        .steps
        .iter()
        .filter(|s| !sb.skip_steps.contains(&s.id))
        .map(|s| {
            let mut s = s.clone();
            if let Some(o) = sb.overrides.get(&s.id) {
                s.body.extend(o.clone());
            }
            s
        })
        .collect();
    let sandboxed = ComponentConfig {
        description: comp.description.clone(),
        steps,
        release: comp.release.clone(),
        vars: merged_vars(comp, sb),
        sandbox: None,
    };
    ArchanistConfig {
        log_level: cfg.log_level.clone(),
        log_dir: cfg.log_dir.clone(),
        log_file_prefix: cfg.log_file_prefix.clone(),
        log_file_level: cfg.log_file_level.clone(),
        log_rotation: cfg.log_rotation,
        state_file: PathBuf::from("state.toml"),
        // `recipe_sync` installs here, away from the real recipes.
        components_dir: dir.join("components"),
        base_dir: dir.to_path_buf(),
        self_component: None,
        components: HashMap::from([(component.to_string(), sandboxed)]),
    }
}

fn container_names(sb: &SandboxConfig, env: &Env) -> Result<Vec<String>> {
    sb.services
        .iter()
        .map(|s| &s.name)
        .chain(&sb.cleanup_containers)
        .map(|n| interp::interpolate(n, env))
        .collect()
}

fn interpolate_all(items: &[String], env: &Env) -> Result<Vec<String>> {
    items.iter().map(|s| interp::interpolate(s, env)).collect()
}

async fn start_service(
    docker: &DockerClient,
    svc: &SandboxService,
    network: Option<&str>,
    env: &Env,
) -> Result<()> {
    let name = interp::interpolate(&svc.name, env)?;
    let mut healthcheck = svc.healthcheck.clone();
    if let Some(hc) = healthcheck.as_mut() {
        hc.test = interpolate_all(&hc.test, env)?;
    }

    if docker.find_container(&name).await?.is_some() {
        println!("[sandbox] service {name} already exists, starting it");
        docker.start_existing(&name).await?;
    } else {
        let image = interp::interpolate(&svc.image, env)?;
        println!("[sandbox] starting service {name} ({image})");
        docker.pull_image(&image).await?;
        docker
            .run_container(ServiceSpec {
                name: name.clone(),
                image,
                cmd: interpolate_all(&svc.command, env)
                    .with_context(|| format!("service '{name}': command"))?,
                env: interpolate_all(&svc.env, env)
                    .with_context(|| format!("service '{name}': env"))?,
                volumes: interpolate_all(&svc.volumes, env)
                    .with_context(|| format!("service '{name}': volumes"))?,
                network: network.map(str::to_owned),
                restart_policy: Some("no".into()),
                healthcheck: healthcheck.clone(),
                ..Default::default()
            })
            .await?;
    }

    if let Some(hc) = &healthcheck {
        println!("[sandbox] waiting for {name} to become healthy");
        docker.wait_healthy(&name, hc).await?;
    }
    Ok(())
}
