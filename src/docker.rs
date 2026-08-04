//! Thin async wrapper around the Docker daemon via [`bollard`].
//!
//! Exposes just the handful of operations `docker_swap` needs: pull an
//! image, stop / remove / (re)create a container, and read a running
//! container's current image reference. Everything else (networks,
//! healthchecks, exec) is deliberately out of scope.

use anyhow::{Context, Result};
use bollard::Docker;
use bollard::models::{
    ContainerCreateBody, ContainerSummary, HostConfig, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use futures_util::StreamExt;
use std::collections::HashMap;
use tracing::{debug, info};

#[derive(Clone)]
pub struct DockerClient {
    client: Docker,
}

impl DockerClient {
    pub fn connect() -> Result<Self> {
        let client =
            Docker::connect_with_local_defaults().context("failed to connect to Docker daemon")?;
        Ok(Self { client })
    }

    pub async fn pull_image(&self, image: &str) -> Result<()> {
        info!("pulling image: {image}");
        let (repo, tag) = split_image_tag(image);
        let options = CreateImageOptions {
            from_image: Some(repo.to_string()),
            tag: Some(tag.to_string()),
            ..Default::default()
        };
        let mut stream = self.client.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            let info = result.context("error pulling image")?;
            if let Some(status) = &info.status {
                debug!("pull: {status}");
            }
        }
        info!("pulled: {image}");
        Ok(())
    }

    pub async fn find_container(&self, name: &str) -> Result<Option<ContainerSummary>> {
        let filters: HashMap<String, Vec<String>> = [("name".into(), vec![name.into()])].into();
        let options = ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        };
        let containers = self
            .client
            .list_containers(Some(options))
            .await
            .context("failed to list containers")?;
        let exact = format!("/{name}");
        Ok(containers
            .into_iter()
            .find(|c| c.names.as_ref().is_some_and(|n| n.contains(&exact))))
    }

    pub async fn container_image(&self, name: &str) -> Result<Option<String>> {
        Ok(self.find_container(name).await?.and_then(|c| c.image))
    }

    pub async fn stop_container(&self, name: &str) -> Result<()> {
        info!("stopping container: {name}");
        let opts = StopContainerOptions {
            t: Some(10),
            ..Default::default()
        };
        match self.client.stop_container(name, Some(opts)).await {
            Ok(()) => Ok(()),
            // 304: already stopped. 404: not found. Both are fine for our use case.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            })
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("stop {name}")),
        }
    }

    pub async fn remove_container(&self, name: &str) -> Result<()> {
        info!("removing container: {name}");
        let opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        match self.client.remove_container(name, Some(opts)).await {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {name}")),
        }
    }

    pub async fn run_container(
        &self,
        name: &str,
        image: &str,
        env: Vec<String>,
        volumes: Vec<String>,
        restart_policy: Option<String>,
    ) -> Result<String> {
        info!("creating container {name} from {image}");
        let host_config = HostConfig {
            binds: Some(volumes),
            restart_policy: restart_policy.map(|p| RestartPolicy {
                name: Some(parse_restart_policy(&p)),
                maximum_retry_count: None,
            }),
            ..Default::default()
        };
        let config = ContainerCreateBody {
            image: Some(image.into()),
            env: Some(env),
            host_config: Some(host_config),
            ..Default::default()
        };
        let opts = CreateContainerOptions {
            name: Some(name.into()),
            platform: String::new(),
        };
        let resp = self
            .client
            .create_container(Some(opts), config)
            .await
            .with_context(|| format!("create {name}"))?;
        self.client
            .start_container(&resp.id, None::<StartContainerOptions>)
            .await
            .with_context(|| format!("start {name}"))?;
        info!(
            "container {name} started (id: {})",
            &resp.id[..12.min(resp.id.len())]
        );
        Ok(resp.id)
    }
}

fn parse_restart_policy(p: &str) -> RestartPolicyNameEnum {
    match p {
        "always" => RestartPolicyNameEnum::ALWAYS,
        "unless-stopped" => RestartPolicyNameEnum::UNLESS_STOPPED,
        "on-failure" => RestartPolicyNameEnum::ON_FAILURE,
        _ => RestartPolicyNameEnum::EMPTY,
    }
}

/// Split `repo/name:tag` -> (`repo/name`, `tag`). Falls back to `latest` if no tag.
/// A `:` inside the repo portion (e.g. `host:5000/name`) is not treated as a tag.
fn split_image_tag(full: &str) -> (&str, &str) {
    match full.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => (repo, tag),
        _ => (full, "latest"),
    }
}

#[cfg(test)]
mod tests {
    use super::split_image_tag;

    #[test]
    fn no_tag_defaults_to_latest() {
        assert_eq!(split_image_tag("nginx"), ("nginx", "latest"));
    }

    #[test]
    fn explicit_tag_is_extracted() {
        assert_eq!(split_image_tag("nginx:1.25"), ("nginx", "1.25"));
    }

    #[test]
    fn registry_port_is_not_mistaken_for_tag() {
        assert_eq!(
            split_image_tag("ghcr.io:5000/foo/bar"),
            ("ghcr.io:5000/foo/bar", "latest")
        );
    }

    #[test]
    fn registry_port_with_explicit_tag() {
        assert_eq!(
            split_image_tag("ghcr.io:5000/foo/bar:v2"),
            ("ghcr.io:5000/foo/bar", "v2")
        );
    }
}
