use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

#[derive(Debug, Default, Deserialize)]
pub struct ArchanistState {
    #[serde(default)]
    pub components: HashMap<String, ComponentState>,
}

#[derive(Debug, Deserialize)]
pub struct ComponentState {
    pub current_version: Option<String>,
    pub previous_version: Option<String>,
    pub last_check: Option<DateTime<Utc>>,
}

impl ArchanistState {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            debug!("state file {} not found, starting fresh", path.display());
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read state file: {}", path.display()))?;
        let state: Self = toml::from_str(&content).context("Failed to parse state file")?;
        debug!(
            "loaded state from {} ({} component(s))",
            path.display(),
            state.components.len()
        );
        Ok(state)
    }
}
