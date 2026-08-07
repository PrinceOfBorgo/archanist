mod config;
mod docker;
mod interp;
mod pipeline;
mod release;
mod state;
mod steps;
mod tui;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand};
use std::path::{Path, PathBuf};
use tracing::{debug, info_span};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

const DEFAULT_CONFIG_TOML: &str = r#"schema = 1

# Console log level: trace | debug | info | warn | error
log_level = "info"

# [Optional] File logging settings (omit log_dir to disable).
# Files are named <log_file_prefix>.<date>.log and written under log_dir.
log_dir = "logs"
log_file_prefix = "archanist"
log_file_level = "debug"
log_rotation = "daily"   # daily | hourly | never

# Path to the state file (relative to this config file, or absolute).
state_file = "state.toml"

# Directory containing component recipe files (relative to this config file, or absolute).
components_dir = "components"

# [optional] Which component file represents this archanist for self-update purposes.
# Must match a filename in components_dir (without .toml extension).
# self_component = "archanist"
"#;

#[derive(Parser)]
#[command(
    name = "archanist",
    arg_required_else_help = true,
    version,
    about = "A modular deployment archanist"
)]
struct Cli {
    /// Path to the archanist configuration file.
    #[arg(short, long, default_value = "config.toml", global = true)]
    config: PathBuf,

    /// Increase console log verbosity (-v = debug, -vv = trace). Overrides config log_level.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a default config file at the target path if it doesn't exist.
    Init,
    /// Print the parsed configuration (useful for troubleshooting).
    Config,
    /// Print current state (versions, last check, blocklist).
    Status {
        /// Only show this component. Omit to show all.
        #[arg(short = 'n', long)]
        component: Option<String>,
    },
    /// Check upstream release feeds for available updates.
    Check {
        /// Only check this component. Omit to check all.
        #[arg(short = 'n', long)]
        component: Option<String>,
    },
    /// Roll a component back by invoking each applied step's rollback in reverse order.
    Rollback {
        /// Name of the component to roll back.
        component: String,
    },
    /// Check for updates and apply them if a newer version is available.
    Update {
        /// Only update this component. Omit to update all.
        #[arg(short = 'n', long)]
        component: Option<String>,
    },
    /// Clear a version from the blocklist so it can be installed again.
    Unblock { component: String, version: String },
    /// List all built-in step kinds.
    StepKinds,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Init and Config don't need the async runtime
    match &cli.command {
        Commands::Init => return run_init(&cli.config),
        Commands::Config => {
            let cfg = config::ArchanistConfig::load(&cli.config)?;
            let state = state::ArchanistState::load(&cfg.state_path())?;
            return tui::run_config_editor(&cfg, &state);
        }
        _ => {}
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("failed to build tokio runtime")?
        .block_on(run_async(cli))
}

async fn run_async(cli: Cli) -> Result<()> {
    let (cfg, _log_guard) = load_and_setup(&cli)?;

    let run_id = format!("run-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let run_span = info_span!("run", run_id = %run_id);
    let _entered = run_span.enter();

    debug!(
        "archanist v{} starting from config {}",
        env!("CARGO_PKG_VERSION"),
        cli.config.display()
    );
    debug!("discovered {} component(s)", cfg.components.len());
    for name in cfg.component_names() {
        debug!(
            "component '{}' has {} step(s)",
            name,
            cfg.components[&name].steps.len()
        );
    }

    match &cli.command {
        Commands::Init | Commands::Config => {
            unreachable!("Init and Config handled before async runtime")
        }
        Commands::Status { component } => {
            let state = state::ArchanistState::load(&cfg.state_path())?;
            run_status(&cfg, &state, component.as_deref())?;
        }
        Commands::Check { component } => {
            let state_path = cfg.state_path();
            let mut state = state::ArchanistState::load(&state_path)?;
            run_check(&cfg, &mut state, component.as_deref()).await?;
            state.save(&state_path)?;
        }
        Commands::Rollback { component } => {
            let state_path = cfg.state_path();
            let mut state = state::ArchanistState::load(&state_path)?;
            let registry = std::sync::Arc::new(steps::builtin_registry());
            run_rollback(&cfg, &mut state, component, &registry).await?;
            state.save(&state_path)?;
        }
        Commands::Update { component } => {
            let state_path = cfg.state_path();
            let mut state = state::ArchanistState::load(&state_path)?;
            let registry = std::sync::Arc::new(steps::builtin_registry());
            let result = run_update(
                &cfg,
                &mut state,
                component.as_deref(),
                &state_path,
                &registry,
            )
            .await;
            state.save(&state_path)?;
            result?;
        }
        Commands::Unblock { component, version } => {
            let state_path = cfg.state_path();
            let mut state = state::ArchanistState::load(&state_path)?;
            run_unblock(&mut state, component, version)?;
            state.save(&state_path)?;
        }
        Commands::StepKinds => {
            let registry = steps::builtin_registry();
            for kind in registry.kinds() {
                println!("{kind}");
            }
        }
    }
    Ok(())
}

fn load_and_setup(cli: &Cli) -> Result<(config::ArchanistConfig, Option<WorkerGuard>)> {
    let cfg = config::ArchanistConfig::load(&cli.config)?;
    let guard = setup_tracing(&cfg, cli.verbose)?;
    Ok((cfg, guard))
}

fn setup_tracing(cfg: &config::ArchanistConfig, verbose: u8) -> Result<Option<WorkerGuard>> {
    let console_level = match verbose {
        0 => cfg.log_level.as_str(),
        1 => "debug",
        _ => "trace",
    };
    let pkg = env!("CARGO_PKG_NAME").replace('-', "_");

    let console_filter =
        EnvFilter::from_default_env().add_directive(format!("{}={}", pkg, console_level).parse()?);
    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(console_filter);

    let (file_layer, guard) = if let Some(log_dir) = &cfg.log_dir {
        std::fs::create_dir_all(log_dir)
            .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;
        let rotation = match cfg.log_rotation {
            config::LogRotation::Daily => Rotation::DAILY,
            config::LogRotation::Hourly => Rotation::HOURLY,
            config::LogRotation::Never => Rotation::NEVER,
        };
        let appender = Builder::new()
            .rotation(rotation)
            .filename_prefix(&cfg.log_file_prefix)
            .filename_suffix("log")
            .build(log_dir)
            .with_context(|| format!("Failed to build file appender in {}", log_dir.display()))?;
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        let file_filter = EnvFilter::from_default_env()
            .add_directive(format!("{}={}", pkg, cfg.log_file_level).parse()?);
        let event_format = fmt::format().with_ansi(false).with_target(true);
        let layer = fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .event_format(event_format)
            .with_filter(file_filter);
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(file_layer)
        .with(console_layer)
        .init();

    Ok(guard)
}

fn run_init(config_path: &Path) -> Result<()> {
    if config_path.exists() {
        println!("Skipped {} (already exists)", config_path.display());
        return Ok(());
    }
    if let Some(parent) = config_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    std::fs::write(config_path, DEFAULT_CONFIG_TOML)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;
    println!("Created {}", config_path.display());
    Ok(())
}

fn run_status(
    cfg: &config::ArchanistConfig,
    state: &state::ArchanistState,
    filter: Option<&str>,
) -> Result<()> {
    if cfg.components.is_empty() {
        println!(
            "(no components configured; looked in {})",
            cfg.components_dir.display()
        );
        return Ok(());
    }

    let names: Vec<String> = if let Some(name) = filter {
        if !cfg.components.contains_key(name) {
            bail!(
                "unknown component '{}' (run `archanist step-kinds` to list built-in step types or check your components directory)",
                name
            );
        }
        vec![name.to_string()]
    } else {
        cfg.component_names()
    };

    for name in names {
        let comp = &cfg.components[&name];
        println!("=== {name} ===");
        if let Some(desc) = &comp.description {
            println!("  description: {desc}");
        }
        match state.components.get(&name) {
            Some(s) => {
                let current = s.current_version.as_deref().unwrap_or("unknown");
                println!("  current:  {}", current);
                println!(
                    "  previous: {}",
                    s.previous_version.as_deref().unwrap_or("none")
                );
                if let Some(latest) = &s.latest_check_version {
                    println!("  latest:   {}", latest);
                    if s.current_version.as_deref() != Some(latest.as_str()) {
                        println!("  UPDATE AVAILABLE: {} -> {}", current, latest);
                    }
                }
                if let Some(t) = s.last_check {
                    println!("  last check: {t}");
                }
                if !s.blocklist.is_empty() {
                    println!("  blocked:  {}", s.blocklist.join(", "));
                }
            }
            None => println!("  (no state yet)"),
        }
        println!();
    }
    Ok(())
}

async fn run_check(
    cfg: &config::ArchanistConfig,
    state: &mut state::ArchanistState,
    filter: Option<&str>,
) -> Result<()> {
    let names: Vec<String> = if let Some(name) = filter {
        if !cfg.components.contains_key(name) {
            bail!(
                "unknown component '{}' (run `archanist list` to see configured components)",
                name
            );
        }
        vec![name.to_string()]
    } else {
        cfg.component_names()
    };

    for name in &names {
        let comp = &cfg.components[name];
        let Some(src) = &comp.release else {
            println!("{}: no [release] source configured, skipping", name);
            continue;
        };
        print!("{}: checking... ", name);
        match src.fetch_latest().await {
            Ok(Some(latest)) => {
                let entry = state.components.entry(name.clone()).or_default();
                let current = entry
                    .current_version
                    .as_deref()
                    .unwrap_or("(none)")
                    .to_string();
                let update_available = match &entry.current_version {
                    Some(c) => release::is_newer(&latest, c),
                    None => true,
                };
                entry.latest_check_version = Some(latest.clone());
                entry.last_check = Some(Utc::now());
                let marker = if update_available {
                    " (update available)"
                } else {
                    ""
                };
                println!("current={}, latest={}{}", current, latest, marker);
            }
            Ok(None) => {
                println!("no stable release available");
            }
            Err(e) => {
                println!("failed: {:#}", e);
            }
        }
    }
    Ok(())
}

async fn run_rollback(
    cfg: &config::ArchanistConfig,
    state: &mut state::ArchanistState,
    component: &str,
    registry: &std::sync::Arc<steps::StepRegistry>,
) -> Result<()> {
    let comp = cfg.components.get(component).with_context(|| {
        format!(
            "unknown component '{}' (run `archanist list` to see configured components)",
            component
        )
    })?;

    let Some(entry) = state.components.get_mut(component) else {
        println!("{}: no state recorded, nothing to roll back", component);
        return Ok(());
    };

    if entry.applied_steps.is_empty() {
        println!(
            "{}: no applied steps recorded, nothing to roll back",
            component
        );
        return Ok(());
    }

    let is_self_update = cfg.self_component.as_deref() == Some(component);

    // Rollback consults each step's payload snapshot rather than its
    // configured template, so we build steps from raw (un-interpolated)
    // config and never spin up a pipeline runner here
    for step_cfg in comp.steps.iter().rev() {
        if let Some(applied) = entry.applied_steps.get(&step_cfg.id).cloned() {
            let step = registry
                .build_raw(step_cfg)
                .with_context(|| format!("failed to build step '{}'", step_cfg.id))?;
            let ctx = steps::StepCtx {
                step_id: step_cfg.id.clone(),
                is_self_update,
            };
            println!(
                "rolling back step '{}' (applied at {})",
                step_cfg.id, applied.applied_at
            );
            step.rollback(&ctx, &applied.payload)
                .await
                .with_context(|| format!("rollback of step '{}' failed", step_cfg.id))?;
            entry.applied_steps.remove(&step_cfg.id);
        }
    }
    Ok(())
}

async fn run_update(
    cfg: &config::ArchanistConfig,
    state: &mut state::ArchanistState,
    filter: Option<&str>,
    state_path: &Path,
    registry: &std::sync::Arc<steps::StepRegistry>,
) -> Result<()> {
    let names: Vec<String> = if let Some(name) = filter {
        if !cfg.components.contains_key(name) {
            bail!(
                "unknown component '{}' (run `archanist list` to see configured components)",
                name
            );
        }
        vec![name.to_string()]
    } else {
        cfg.component_names()
    };

    let mut failures: Vec<String> = Vec::new();

    for name in &names {
        let comp = &cfg.components[name];
        let Some(src) = &comp.release else {
            println!("{}: no [release] source configured, skipping", name);
            continue;
        };

        print!("{}: checking... ", name);
        let latest = match src.fetch_latest().await {
            Ok(Some(v)) => v,
            Ok(None) => {
                println!("no stable release available, skipping");
                continue;
            }
            Err(e) => {
                println!("failed: {:#}", e);
                continue;
            }
        };

        let entry = state.components.entry(name.clone()).or_default();
        entry.latest_check_version = Some(latest.clone());
        entry.last_check = Some(Utc::now());

        let up_to_date = match &entry.current_version {
            Some(c) => !release::is_newer(&latest, c),
            None => false,
        };
        if up_to_date {
            println!("up to date at {}", latest);
            continue;
        }
        if entry.blocklist.iter().any(|v| v == &latest) {
            println!(
                "version {} is blocked (unblock with `archanist unblock {} {}`)",
                latest, name, latest
            );
            continue;
        }

        let current = entry
            .current_version
            .as_deref()
            .unwrap_or("(none)")
            .to_string();
        println!("updating {} -> {}", current, latest);

        let is_self_update = cfg.self_component.as_deref() == Some(name.as_str());
        let pipeline = pipeline::Pipeline::build(name, comp, is_self_update, registry.clone());
        let component_name = name.clone();

        // Seed the pipeline env with built-in vars and per-component `[vars]`
        let mut base_env = interp::Env::new();
        base_env.insert("version".into(), latest.clone());
        base_env.insert("current_version".into(), current.clone());
        base_env.insert("component".into(), name.clone());
        let initial_env = interp::expand_component_vars(&base_env, &comp.vars);

        let result = pipeline
            .run(initial_env, |step_cfg, outcome| {
                let applied = state::AppliedStep {
                    kind: step_cfg.kind.clone(),
                    applied_at: Utc::now(),
                    payload: outcome.payload.clone(),
                };
                state
                    .components
                    .entry(component_name.clone())
                    .or_default()
                    .applied_steps
                    .insert(step_cfg.id.clone(), applied);
                state.save(state_path)
            })
            .await;

        match result {
            Ok(()) => {
                let entry = state.components.entry(name.clone()).or_default();
                entry.previous_version = entry.current_version.take();
                entry.current_version = Some(latest.clone());
                println!("{}: updated to {}", name, latest);
            }
            Err(e) => {
                println!("{}: update failed: {:#}", name, e);
                let entry = state.components.entry(name.clone()).or_default();
                if !entry.blocklist.iter().any(|v| v == &latest) {
                    entry.blocklist.push(latest.clone());
                }
                failures.push(name.clone());
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        bail!("update failed for: {}", failures.join(", "))
    }
}

fn run_unblock(state: &mut state::ArchanistState, component: &str, version: &str) -> Result<()> {
    let entry = state.components.get_mut(component).with_context(|| {
        format!(
            "unknown component '{}' (run `archanist list` to see configured components)",
            component
        )
    })?;
    let before = entry.blocklist.len();
    entry.blocklist.retain(|v| v != version);
    if entry.blocklist.len() == before {
        println!(
            "{}: version {} was not in the blocklist",
            component, version
        );
    } else {
        println!("{}: unblocked version {}", component, version);
    }
    Ok(())
}
