use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ArchanistState {
    #[serde(default)]
    pub components: HashMap<String, ComponentState>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ComponentState {
    pub current_version: Option<String>,
    pub previous_version: Option<String>,
    pub last_check: Option<DateTime<Utc>>,
}

impl ArchanistState {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read state file: {}", path.display()))?;
        toml::from_str(&content).context("Failed to parse state file")
    }

    pub fn print_summary(&self) {
        if self.components.is_empty() {
            println!("(no components tracked yet)");
            return;
        }
        let mut names: Vec<&String> = self.components.keys().collect();
        names.sort();
        for name in names {
            let s = &self.components[name];
            println!("=== {name} ===");
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
            println!();
        }
    }
}
