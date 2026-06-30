use crate::config::StepConfig;
use crate::interp::{Env, interpolate};
use crate::steps::{ExecuteFuture, Step};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, info};

pub struct HttpHealth {
    id: String,
    url: String,
    expected_status: u16,
    timeout: Duration,
    interval: Duration,
}

#[derive(Deserialize)]
struct HttpHealthRaw {
    url: String,
    #[serde(default = "default_expected_status")]
    expected_status: u16,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
    #[serde(default = "default_interval_secs")]
    interval_secs: u64,
}

fn default_expected_status() -> u16 {
    200
}
fn default_timeout_secs() -> u64 {
    60
}
fn default_interval_secs() -> u64 {
    2
}

impl Step for HttpHealth {
    fn from_config(cfg: &StepConfig) -> Result<Self> {
        let raw: HttpHealthRaw = toml::Value::Table(cfg.extra.clone())
            .try_into()
            .with_context(|| format!("step '{}': invalid http_health config", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            url: raw.url,
            expected_status: raw.expected_status,
            timeout: Duration::from_secs(raw.timeout_secs),
            interval: Duration::from_secs(raw.interval_secs),
        })
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &str {
        "http_health"
    }

    fn describe(&self) -> String {
        format!(
            "poll {} until HTTP {} (timeout {}s, interval {}s)",
            self.url,
            self.expected_status,
            self.timeout.as_secs(),
            self.interval.as_secs()
        )
    }

    fn execute<'a>(&'a self, env: &'a mut Env) -> ExecuteFuture<'a> {
        Box::pin(async move {
            let url = interpolate(&self.url, env)
                .with_context(|| format!("step '{}': failed to interpolate url", self.id))?;
            info!(
                "[{}] polling {} for HTTP {}",
                self.id, url, self.expected_status
            );
            let client = reqwest::Client::builder()
                .user_agent(concat!("archanist/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(10))
                .build()
                .with_context(|| format!("step '{}': failed to build HTTP client", self.id))?;

            let start = Instant::now();
            loop {
                match client.get(&url).send().await {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        if status == self.expected_status {
                            info!(
                                "[{}] got HTTP {} from {} after {:.1}s",
                                self.id,
                                status,
                                url,
                                start.elapsed().as_secs_f32()
                            );
                            return Ok(());
                        }
                        debug!(
                            "[{}] got HTTP {}, expected {}, retrying",
                            self.id, status, self.expected_status
                        );
                    }
                    Err(e) => {
                        debug!("[{}] request failed ({}), retrying", self.id, e);
                    }
                }
                if start.elapsed() >= self.timeout {
                    bail!(
                        "step '{}': did not get HTTP {} from {} within {}s",
                        self.id,
                        self.expected_status,
                        url,
                        self.timeout.as_secs()
                    );
                }
                sleep(self.interval).await;
            }
        })
    }
}
