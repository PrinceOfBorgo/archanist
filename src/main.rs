mod config;
mod pipeline;
mod state;
mod steps;

use anyhow::{Context, Result};
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

# Optional file logging (omit log_dir to disable).
# Files are named <log_file_prefix>.<date>.log and written under log_dir.
log_dir = "logs"
log_file_prefix = "archanist"
log_file_level = "debug"
log_rotation = "daily"   # daily | hourly | never

# Path to the state file (relative to this config file, or absolute).
state_file = "state.toml"

# Directory containing component recipe files (relative to this config file, or absolute).
components_dir = "components"
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
    /// List configured components.
    List,
    /// Print current state.
    Status,
    /// Print the planned steps for a component without executing them.
    DryRun {
        /// Name of the component to plan.
        component: String,
    },
    /// Execute all steps of a component in order.
    Run {
        /// Name of the component to run.
        component: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if matches!(cli.command, Commands::Init) {
        return run_init(&cli.config);
    }

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
        Commands::Init => unreachable!(),
        Commands::Config => {
            println!("{:#?}", cfg);
        }
        Commands::List => run_list(&cfg),
        Commands::Status => {
            let state = state::ArchanistState::load(&cfg.state_path())?;
            run_status(&cfg, &state);
        }
        Commands::DryRun { component } => {
            let pipeline = build_component_pipeline(&cfg, component)?;
            pipeline.dry_run();
        }
        Commands::Run { component } => {
            let pipeline = build_component_pipeline(&cfg, component)?;
            pipeline.run()?;
        }
    }
    Ok(())
}

fn build_component_pipeline(
    cfg: &config::ArchanistConfig,
    component: &str,
) -> Result<pipeline::Pipeline> {
    let comp = cfg.components.get(component).with_context(|| {
        format!(
            "unknown component '{}' (run `archanist list` to see configured components)",
            component
        )
    })?;
    pipeline::Pipeline::build(component, comp)
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

fn run_list(cfg: &config::ArchanistConfig) {
    if cfg.components.is_empty() {
        println!(
            "(no components configured; looked in {})",
            cfg.components_dir.display()
        );
        return;
    }
    for name in cfg.component_names() {
        let comp = &cfg.components[&name];
        println!("=== {name} ===");
        if let Some(desc) = &comp.description {
            println!("  description: {desc}");
        }
        println!("  steps ({}):", comp.steps.len());
        for step in &comp.steps {
            println!("    - {} [{}]", step.id, step.kind);
        }
        println!();
    }
}

fn run_status(cfg: &config::ArchanistConfig, state: &state::ArchanistState) {
    if cfg.components.is_empty() {
        println!(
            "(no components configured; looked in {})",
            cfg.components_dir.display()
        );
        return;
    }
    for name in cfg.component_names() {
        println!("=== {name} ===");
        match state.components.get(&name) {
            Some(s) => {
                println!(
                    "  current:  {}",
                    s.current_version.as_deref().unwrap_or("unknown")
                );
                println!(
                    "  previous: {}",
                    s.previous_version.as_deref().unwrap_or("none")
                );
                if let Some(t) = s.last_check {
                    println!("  last check: {t}");
                }
            }
            None => println!("  (no state yet)"),
        }
        println!();
    }
}
