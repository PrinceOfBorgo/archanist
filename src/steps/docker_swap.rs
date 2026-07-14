use crate::config::StepConfig;
use crate::docker::DockerClient;
use crate::interp::interpolate;
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result};
use serde::Deserialize;

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

    fn describe(&self) -> String {
        let self_note = if self.self_update { " [self]" } else { "" };
        format!(
            "swap container {} to {} (restart={}){}",
            self.container,
            Self::full_image(&self.image, &self.tag),
            self.restart_policy,
            self_note
        )
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

            docker.pull_image(&full_image).await?;
            docker.stop_container(&container).await?;
            docker.remove_container(&container).await?;
            docker
                .run_container(
                    &container,
                    &full_image,
                    env,
                    volumes,
                    Some(self.restart_policy.clone()),
                )
                .await?;

            Ok(StepOutcome {
                exit_after: self.self_update,
                ..Default::default()
            })
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
}
