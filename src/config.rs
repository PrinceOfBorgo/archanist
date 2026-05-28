use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const SUPPORTED_MAIN_SCHEMAS: &[u32] = &[1];

#[derive(Debug)]
pub struct ArchanistConfig {
    pub log_level: String,
    pub state_file: PathBuf,
    pub base_dir: PathBuf,
}

#[derive(Deserialize)]
struct MainConfigFile {
    // Catch the schema as an Option so Serde doesn't throw a generic error if it's missing
    schema: Option<u32>,

    #[serde(default = "default_log_level")]
    log_level: String,

    #[serde(default = "default_state_file")]
    state_file: PathBuf,
}

fn default_log_level() -> String {
    "info".into()
}
fn default_state_file() -> PathBuf {
    PathBuf::from("state.toml")
}

impl ArchanistConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let main: MainConfigFile = toml::from_str(&content)
            .with_context(|| format!("Failed to parse archanist config at {}", path.display()))?;

        // Check that schema field is present and valid
        let schema = main.schema.ok_or_else(|| {
            anyhow::anyhow!(
                "{}: missing or invalid `schema` field. Every archanist config \
                 file must declare `schema = <integer>` at the top level.",
                path.display()
            )
        })?;

        // Verify the schema version is supported
        if !SUPPORTED_MAIN_SCHEMAS.contains(&schema) {
            bail!(
                "{}: unsupported main config schema {}. This binary supports {:?}.",
                path.display(),
                schema,
                SUPPORTED_MAIN_SCHEMAS,
            );
        }

        let base_dir = {
            let parent = path.parent().unwrap_or(Path::new(""));
            if parent.is_absolute() {
                parent.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(parent))
                    .unwrap_or_else(|_| parent.to_path_buf())
            }
        };

        Ok(ArchanistConfig {
            log_level: main.log_level,
            state_file: main.state_file,
            base_dir,
        })
    }

    pub fn resolve_path(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base_dir.join(p)
        }
    }

    pub fn state_path(&self) -> PathBuf {
        self.resolve_path(&self.state_file)
    }
}
