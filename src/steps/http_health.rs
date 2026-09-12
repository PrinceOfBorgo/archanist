//! `http_health`: poll an HTTP endpoint until it returns the expected
//! status code. Used after `docker_swap` to gate on the new container
//! actually being ready before the pipeline moves on.

use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, info};

pub struct HttpHealth {
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

impl HttpHealth {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: HttpHealthRaw = body.try_into().context("invalid http_health config")?;
        Ok(Self {
            url: raw.url,
            expected_status: raw.expected_status,
            timeout: Duration::from_secs(raw.timeout_secs),
            interval: Duration::from_secs(raw.interval_secs),
        })
    }
}

impl Step for HttpHealth {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let url = &self.url;
            info!("[{}] polling {} for HTTP {}", id, url, self.expected_status);
            let client = reqwest::Client::builder()
                .user_agent(concat!("archanist/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(10))
                .build()
                .with_context(|| format!("step '{}': failed to build HTTP client", id))?;

            let start = Instant::now();
            loop {
                match client.get(url).send().await {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        if status == self.expected_status {
                            info!(
                                "[{}] got HTTP {} from {} after {:.1}s",
                                id,
                                status,
                                url,
                                start.elapsed().as_secs_f32()
                            );
                            return Ok(StepOutcome::default());
                        }
                        debug!(
                            "[{}] got HTTP {}, expected {}, retrying",
                            id, status, self.expected_status
                        );
                    }
                    Err(e) => {
                        debug!("[{}] request failed ({}), retrying", id, e);
                    }
                }
                if start.elapsed() >= self.timeout {
                    bail!(
                        "step '{}': did not get HTTP {} from {} within {}s",
                        id,
                        self.expected_status,
                        url,
                        self.timeout.as_secs()
                    );
                }
                sleep(self.interval).await;
            }
        })
    }

    fn is_satisfied<'a>(&'a self, _ctx: &'a StepCtx) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            // Single quick probe: if the endpoint already returns the
            // expected status we consider the step satisfied. Any
            // network or status mismatch means "would run".
            let client = match reqwest::Client::builder()
                .user_agent(concat!("archanist/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(3))
                .build()
            {
                Ok(c) => c,
                Err(_) => return Ok(false),
            };
            match client.get(&self.url).send().await {
                Ok(resp) => Ok(resp.status().as_u16() == self.expected_status),
                Err(_) => Ok(false),
            }
        })
    }
}
