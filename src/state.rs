use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ArchanistState {
    #[serde(default)]
    pub components: HashMap<String, ComponentState>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ComponentState {
    pub current_version: Option<String>,
    pub previous_version: Option<String>,
    pub latest_check_version: Option<String>,
    pub last_check: Option<DateTime<Utc>>,
    #[serde(default)]
    pub applied_steps: HashMap<String, AppliedStep>,
    #[serde(default)]
    pub blocklist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedStep {
    pub kind: String,
    pub applied_at: DateTime<Utc>,
    pub payload: toml::Value,
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

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("Failed to serialize state to TOML")?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, &content)
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| {
            format!(
                "Failed to move {} into place at {}",
                tmp.display(),
                path.display()
            )
        })?;
        debug!("saved state to {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applied_step_roundtrip() {
        let payload =
            toml::from_str::<toml::Value>("previous_image = \"nginx:1.24\"\ncontainer = \"web\"\n")
                .unwrap();
        let a = AppliedStep {
            kind: "docker_swap".into(),
            applied_at: Utc::now(),
            payload,
        };
        let toml_str = toml::to_string(&a).unwrap();
        let back: AppliedStep = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.kind, "docker_swap");
        assert_eq!(
            back.payload.get("previous_image").and_then(|v| v.as_str()),
            Some("nginx:1.24")
        );
    }

    #[test]
    fn component_state_default_has_empty_applied_steps() {
        let s = ComponentState::default();
        assert!(s.applied_steps.is_empty());
    }

    #[test]
    fn state_with_applied_steps_roundtrips() {
        let mut state = ArchanistState::default();
        let mut comp = ComponentState {
            current_version: Some("1.0.0".into()),
            ..Default::default()
        };
        comp.applied_steps.insert(
            "swap".into(),
            AppliedStep {
                kind: "docker_swap".into(),
                applied_at: DateTime::parse_from_rfc3339("2026-07-18T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                payload: toml::Value::Table(Default::default()),
            },
        );
        state.components.insert("web".into(), comp);
        let s = toml::to_string(&state).unwrap();
        let back: ArchanistState = toml::from_str(&s).unwrap();
        assert_eq!(
            back.components["web"].current_version.as_deref(),
            Some("1.0.0")
        );
        assert!(back.components["web"].applied_steps.contains_key("swap"));
    }

    #[test]
    fn state_with_blocklist_roundtrips() {
        let mut state = ArchanistState::default();
        let comp = ComponentState {
            current_version: Some("1.0.0".into()),
            blocklist: vec!["1.0.5".into(), "1.0.6-alpha".into()],
            ..Default::default()
        };
        state.components.insert("web".into(), comp);
        let s = toml::to_string(&state).unwrap();
        let back: ArchanistState = toml::from_str(&s).unwrap();
        assert_eq!(back.components["web"].blocklist.len(), 2);
        assert!(back.components["web"].blocklist.contains(&"1.0.5".into()));
    }

    #[test]
    fn blocklist_defaults_to_empty_when_absent_from_toml() {
        // Older state files without blocklist should still deserialize.
        let s = r#"
[components.web]
current_version = "1.0.0"
"#;
        let back: ArchanistState = toml::from_str(s).unwrap();
        assert!(back.components["web"].blocklist.is_empty());
    }
}
