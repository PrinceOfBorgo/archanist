use crate::config::StepConfig;
use crate::interp::{Env, interpolate};
use crate::steps::{ExecuteFuture, Step};
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use tracing::{debug, info};

pub struct ParseText {
    id: String,
    source: String,
    regex: Regex,
}

#[derive(Deserialize)]
struct ParseTextRaw {
    source: String,
    regex: String,
}

impl Step for ParseText {
    fn from_config(cfg: &StepConfig) -> Result<Self> {
        let raw: ParseTextRaw = toml::Value::Table(cfg.extra.clone())
            .try_into()
            .with_context(|| format!("step '{}': invalid parse_text config", cfg.id))?;
        let regex =
            Regex::new(&raw.regex).with_context(|| format!("step '{}': invalid regex", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            source: raw.source,
            regex,
        })
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &str {
        "parse_text"
    }

    fn describe(&self) -> String {
        format!("scan {} with /{}/", self.source, self.regex.as_str())
    }

    fn execute<'a>(&'a self, env: &'a mut Env) -> ExecuteFuture<'a> {
        Box::pin(async move {
            let source = interpolate(&self.source, env)
                .with_context(|| format!("step '{}': failed to interpolate source", self.id))?;
            info!(
                "[{}] scanning {} with /{}/",
                self.id,
                source,
                self.regex.as_str()
            );
            let content = tokio::fs::read_to_string(&source)
                .await
                .with_context(|| format!("step '{}': failed to read {}", self.id, source))?;

            let caps = self.regex.captures(&content).ok_or_else(|| {
                anyhow::anyhow!("step '{}': regex did not match in {}", self.id, source)
            })?;

            let mut exported = Vec::new();
            for name in self.regex.capture_names().flatten() {
                if let Some(m) = caps.name(name) {
                    let value = m.as_str().to_string();
                    debug!("[{}] captured {} = {:?}", self.id, name, value);
                    env.insert(name.to_string(), value);
                    exported.push(name);
                }
            }
            info!(
                "[{}] exported {} variable(s) from {}: {:?}",
                self.id,
                exported.len(),
                source,
                exported
            );
            Ok(())
        })
    }
}
