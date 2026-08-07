//! `docker_swap`: pull a new image and (re)create the target container.
//!
//! On rollback the container is restored to the previously-recorded
//! image, if one was captured at apply time. When `self = true` (or
//! the component is the configured `self_component`), the step signals
//! `exit_after` so the pipeline stops cleanly and the new container
//! image can take over.

use crate::docker::DockerClient;
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

pub struct DockerSwap {
    image: String,
    container: String,
    tag: String,
    volumes: Vec<String>,
    env: Vec<String>,
    restart_policy: String,
    self_update: bool,
}

#[derive(Deserialize)]
struct DockerSwapRaw {
    image: String,
    container: String,
    #[serde(default = "default_tag")]
    tag: String,
    #[serde(default)]
    volumes: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default = "default_restart_policy")]
    restart_policy: String,
    /// Marks this as the archanist's own container. Signals exit_after so the
    /// pipeline stops cleanly and the new container image takes over.
    #[serde(default, rename = "self")]
    self_update: bool,
}

fn default_tag() -> String {
    "latest".into()
}
fn default_restart_policy() -> String {
    "unless-stopped".into()
}

impl DockerSwap {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: DockerSwapRaw = body.try_into().context("invalid docker_swap config")?;
        Ok(Self {
            image: raw.image,
            container: raw.container,
            tag: raw.tag,
            volumes: raw.volumes,
            env: raw.env,
            restart_policy: raw.restart_policy,
            self_update: raw.self_update,
        })
    }

    fn full_image(image: &str, tag: &str) -> String {
        if image.contains(':') {
            image.to_string()
        } else {
            format!("{image}:{tag}")
        }
    }
}

/// Snapshot recorded in state at apply time so rollback can restore the
/// container without needing the pipeline's interpolation vars.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SwapPayload {
    container: String,
    previous_image: Option<String>,
    env: Vec<String>,
    volumes: Vec<String>,
    restart_policy: String,
}

impl Step for DockerSwap {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let full_image = Self::full_image(&self.image, &self.tag);
            let docker = DockerClient::connect()?;

            let previous_image = docker.container_image(&self.container).await.ok().flatten();

            docker.pull_image(&full_image).await?;
            docker.stop_container(&self.container).await?;
            docker.remove_container(&self.container).await?;
            docker
                .run_container(
                    &self.container,
                    &full_image,
                    self.env.clone(),
                    self.volumes.clone(),
                    Some(self.restart_policy.clone()),
                )
                .await?;

            let payload = SwapPayload {
                container: self.container.clone(),
                previous_image,
                env: self.env.clone(),
                volumes: self.volumes.clone(),
                restart_policy: self.restart_policy.clone(),
            };
            let payload_val = toml::Value::try_from(&payload)
                .with_context(|| format!("step '{}': failed to serialize payload", id))?;

            Ok(StepOutcome {
                exit_after: self.self_update || ctx.is_self_update,
                payload: payload_val,
                ..Default::default()
            })
        })
    }

    fn rollback<'a>(
        &'a self,
        ctx: &'a StepCtx,
        payload: &'a toml::Value,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let pl: SwapPayload = payload.clone().try_into().unwrap_or_default();
            let Some(prev) = pl.previous_image else {
                warn!(
                    "step '{}': no previous image recorded for {}, cannot rollback",
                    id, pl.container
                );
                return Ok(());
            };
            let docker = DockerClient::connect()?;
            docker.pull_image(&prev).await?;
            docker.stop_container(&pl.container).await?;
            docker.remove_container(&pl.container).await?;
            docker
                .run_container(
                    &pl.container,
                    &prev,
                    pl.env,
                    pl.volumes,
                    Some(pl.restart_policy),
                )
                .await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_image_appends_tag_when_missing() {
        assert_eq!(DockerSwap::full_image("nginx", "1.25"), "nginx:1.25");
    }

    #[test]
    fn full_image_keeps_existing_tag() {
        assert_eq!(
            DockerSwap::full_image("nginx:alpine", "1.25"),
            "nginx:alpine"
        );
    }

    #[test]
    fn full_image_treats_registry_port_as_tag_marker() {
        // If the image string contains ':' we treat it as already-tagged and
        // don't append. The docker client's split_image_tag handles the actual
        // registry-port vs tag distinction at pull time.
        assert_eq!(
            DockerSwap::full_image("ghcr.io:5000/foo/bar", "v2"),
            "ghcr.io:5000/foo/bar"
        );
    }

    #[test]
    fn swap_payload_roundtrip_with_previous() {
        let p = SwapPayload {
            container: "web".into(),
            previous_image: Some("nginx:1.24".into()),
            env: vec!["FOO=bar".into()],
            volumes: vec!["/data:/data".into()],
            restart_policy: "unless-stopped".into(),
        };
        let s = toml::to_string(&p).unwrap();
        let back: SwapPayload = toml::from_str(&s).unwrap();
        assert_eq!(back.container, "web");
        assert_eq!(back.previous_image.as_deref(), Some("nginx:1.24"));
        assert_eq!(back.env, vec!["FOO=bar"]);
    }

    #[test]
    fn swap_payload_roundtrip_without_previous() {
        let p = SwapPayload {
            container: "web".into(),
            previous_image: None,
            env: vec![],
            volumes: vec![],
            restart_policy: "no".into(),
        };
        let s = toml::to_string(&p).unwrap();
        let back: SwapPayload = toml::from_str(&s).unwrap();
        assert_eq!(back.container, "web");
        assert!(back.previous_image.is_none());
    }
}
