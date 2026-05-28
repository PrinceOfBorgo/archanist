mod config;
mod state;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand};
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_TOML: &str = r#"schema = 1

# Log level: trace | debug | info | warn | error
log_level = "info"

# Path to the state file (relative to this config file, or absolute).
state_file = "state.toml"
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

    /// Increase log verbosity (-v = debug, -vv = trace). Overrides config log_level.
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
    /// Print current state.
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Init => run_init(&cli.config),
        Commands::Config => {
            let cfg = load_and_setup(&cli)?;
            println!("{:#?}", cfg);
            Ok(())
        }
        Commands::Status => {
            let cfg = load_and_setup(&cli)?;
            let state = state::ArchanistState::load(&cfg.state_path())?;
            state.print_summary();
            Ok(())
        }
    }
}

fn load_and_setup(cli: &Cli) -> Result<config::ArchanistConfig> {
    let cfg = config::ArchanistConfig::load(&cli.config)?;
    setup_tracing(&cfg.log_level, cli.verbose)?;
    Ok(cfg)
}

fn setup_tracing(config_level: &str, verbose: u8) -> Result<()> {
    let level = match verbose {
        0 => config_level,
        1 => "debug",
        _ => "trace",
    };
    let default_directive = format!("{}={}", env!("CARGO_PKG_NAME").replace('-', "_"), level);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(default_directive.parse()?),
        )
        .init();
    Ok(())
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
