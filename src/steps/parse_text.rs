//! `parse_text`: read a text file, extract data with a regex, and
//! publish the named captures as pipeline vars for subsequent steps.
//!
//! Named capture groups become variables of the same name in the
//! outgoing [`StepOutcome::exported_vars`].

use crate::interp::Env;
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use tracing::{debug, info};

pub struct ParseText {
    source: String,
    regex: Regex,
}

#[derive(Deserialize)]
struct ParseTextRaw {
    source: String,
    regex: String,
}

impl ParseText {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: ParseTextRaw = body.try_into().context("invalid parse_text config")?;
        let regex = Regex::new(&raw.regex).context("invalid regex")?;
        Ok(Self {
            source: raw.source,
            regex,
        })
    }
}

impl Step for ParseText {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let source = &self.source;
            info!(
                "[{}] scanning {} with /{}/",
                id,
                source,
                self.regex.as_str()
            );
            let content = tokio::fs::read_to_string(source)
                .await
                .with_context(|| format!("step '{}': failed to read {}", id, source))?;

            let caps = self.regex.captures(&content).ok_or_else(|| {
                anyhow::anyhow!("step '{}': regex did not match in {}", id, source)
            })?;

            let mut exported_vars = Env::new();
            for name in self.regex.capture_names().flatten() {
                if let Some(m) = caps.name(name) {
                    let value = m.as_str().to_string();
                    debug!("[{}] captured {} = {:?}", id, name, value);
                    exported_vars.insert(name.to_string(), value);
                }
            }
            info!(
                "[{}] exported {} variable(s) from {}",
                id,
                exported_vars.len(),
                source
            );
            Ok(StepOutcome {
                exported_vars,
                ..Default::default()
            })
        })
    }
}
