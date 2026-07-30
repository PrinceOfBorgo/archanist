use crate::config::StepConfig;
use crate::docker::DockerClient;
use crate::interp::interpolate;
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

pub struct DockerSwap {
    id: String,
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
    fn from_config(cfg: &StepConfig) -> Result<Self> {
        let raw: DockerSwapRaw = toml::Value::Table(cfg.extra.clone())
            .try_into()
            .with_context(|| format!("step '{}': invalid docker_swap config", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            image: raw.image,
            container: raw.container,
            tag: raw.tag,
            volumes: raw.volumes,
            env: raw.env,
            restart_policy: raw.restart_policy,
            self_update: raw.self_update,
        })
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &str {
        "docker_swap"
    }

    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let image = interpolate(&self.image, &ctx.vars)
                .with_context(|| format!("step '{}': failed to interpolate image", self.id))?;
            let container = interpolate(&self.container, &ctx.vars)
                .with_context(|| format!("step '{}': failed to interpolate container", self.id))?;
            let tag = interpolate(&self.tag, &ctx.vars)
                .with_context(|| format!("step '{}': failed to interpolate tag", self.id))?;
            let mut env: Vec<String> = Vec::with_capacity(self.env.len());
            for (i, e) in self.env.iter().enumerate() {
                env.push(interpolate(e, &ctx.vars).with_context(|| {
                    format!("step '{}': failed to interpolate env[{}]", self.id, i)
                })?);
            }
            let mut volumes: Vec<String> = Vec::with_capacity(self.volumes.len());
            for (i, v) in self.volumes.iter().enumerate() {
                volumes.push(interpolate(v, &ctx.vars).with_context(|| {
                    format!("step '{}': failed to interpolate volumes[{}]", self.id, i)
                })?);
            }

            let full_image = Self::full_image(&image, &tag);
            let docker = DockerClient::connect()?;

            let previous_image = docker.container_image(&container).await.ok().flatten();

            docker.pull_image(&full_image).await?;
            docker.stop_container(&container).await?;
            docker.remove_container(&container).await?;
            docker
                .run_container(
                    &container,
                    &full_image,
                    env.clone(),
                    volumes.clone(),
                    Some(self.restart_policy.clone()),
                )
                .await?;

            let payload = SwapPayload {
                container: container.clone(),
                previous_image,
                env,
                volumes,
                restart_policy: self.restart_policy.clone(),
            };
            let payload_val = toml::Value::try_from(&payload)
                .with_context(|| format!("step '{}': failed to serialize payload", self.id))?;

            Ok(StepOutcome {
                exit_after: self.self_update || ctx.is_self_update,
                payload: payload_val,
                ..Default::default()
            })
        })
    }

    fn rollback<'a>(
        &'a self,
        _ctx: &'a StepCtx,
        payload: &'a toml::Value,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let pl: SwapPayload = payload.clone().try_into().unwrap_or_default();
            let Some(prev) = pl.previous_image else {
                warn!(
                    "step '{}': no previous image recorded for {}, cannot rollback",
                    self.id, pl.container
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
