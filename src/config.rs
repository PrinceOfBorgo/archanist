use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SUPPORTED_MAIN_SCHEMAS: &[u32] = &[1];
const SUPPORTED_COMPONENT_SCHEMAS: &[u32] = &[1];

#[derive(Debug)]
pub struct ArchanistConfig {
    pub log_level: String,
    pub log_dir: Option<PathBuf>,
    pub log_file_prefix: String,
    pub log_file_level: String,
    pub log_rotation: LogRotation,
    pub state_file: PathBuf,
    pub components_dir: PathBuf,
    pub base_dir: PathBuf,
    pub components: HashMap<String, ComponentConfig>,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogRotation {
    #[default]
    Daily,
    Hourly,
    Never,
}

#[derive(Debug)]
pub struct ComponentConfig {
    pub description: Option<String>,
    pub steps: Vec<StepConfig>,
}

#[derive(Debug, Deserialize)]
pub struct StepConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Deserialize)]
struct MainConfigFile {
    // Catch the schema as an Option so Serde doesn't throw a generic error if it's missing
    schema: Option<u32>,

    #[serde(default = "default_log_level")]
    log_level: String,

    #[serde(default)]
    log_dir: Option<PathBuf>,

    #[serde(default = "default_log_file_prefix")]
    log_file_prefix: String,

    #[serde(default = "default_log_file_level")]
    log_file_level: String,

    #[serde(default)]
    log_rotation: LogRotation,

    #[serde(default = "default_state_file")]
    state_file: PathBuf,

    #[serde(default = "default_components_dir")]
    components_dir: PathBuf,
}

#[derive(Deserialize)]
struct ComponentConfigFile {
    schema: Option<u32>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    steps: Vec<StepConfig>,
}

fn default_log_level() -> String {
    "info".into()
}
fn default_log_file_prefix() -> String {
    "archanist".into()
}
fn default_log_file_level() -> String {
    "debug".into()
}
fn default_state_file() -> PathBuf {
    PathBuf::from("state.toml")
}
fn default_components_dir() -> PathBuf {
    PathBuf::from("components")
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

        let components_dir = if main.components_dir.is_absolute() {
            main.components_dir.clone()
        } else {
            base_dir.join(&main.components_dir)
        };
        let components = Self::load_components(&components_dir)?;

        let log_dir = main
            .log_dir
            .map(|d| if d.is_absolute() { d } else { base_dir.join(d) });

        Ok(ArchanistConfig {
            log_level: main.log_level,
            log_dir,
            log_file_prefix: main.log_file_prefix,
            log_file_level: main.log_file_level,
            log_rotation: main.log_rotation,
            state_file: main.state_file,
            components_dir,
            base_dir,
            components,
        })
    }

    fn load_components(dir: &Path) -> Result<HashMap<String, ComponentConfig>> {
        let mut map = HashMap::new();
        if !dir.exists() {
            return Ok(map);
        }
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read components directory: {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .with_context(|| format!("Invalid component file name: {}", path.display()))?;

            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read component file: {}", path.display()))?;
            let file: ComponentConfigFile = toml::from_str(&content)
                .with_context(|| format!("Failed to parse component file: {}", path.display()))?;

            let schema = file.schema.ok_or_else(|| {
                anyhow::anyhow!(
                    "{}: missing `schema` field. Every component file must declare \
                     `schema = <integer>` at the top level.",
                    path.display()
                )
            })?;
            if !SUPPORTED_COMPONENT_SCHEMAS.contains(&schema) {
                bail!(
                    "{}: unsupported component schema {}. This binary supports {:?}.",
                    path.display(),
                    schema,
                    SUPPORTED_COMPONENT_SCHEMAS,
                );
            }

            let mut seen = std::collections::HashSet::new();
            for step in &file.steps {
                if !seen.insert(step.id.clone()) {
                    bail!("{}: duplicate step id '{}'", path.display(), step.id);
                }
            }

            map.insert(
                name,
                ComponentConfig {
                    description: file.description,
                    steps: file.steps,
                },
            );
        }
        Ok(map)
    }

    pub fn component_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.components.keys().cloned().collect();
        names.sort();
        names
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
