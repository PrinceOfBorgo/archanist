//! Thin async wrapper around the Docker daemon via [`bollard`].
//!
//! Exposes the handful of operations the pipeline needs: pull an image,
//! stop / remove / (re)create a long-lived container, read a running
//! container's current image reference, and run a one-shot helper
//! container to completion. The last one lets recipes reach for
//! component-specific tools (a DB client, `unzip`, ...) by naming their
//! images instead of baking them into the archanist image.

use anyhow::{Context, Result};
use bollard::Docker;
use bollard::models::{
    ContainerCreateBody, ContainerSummary, HostConfig, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    AttachContainerOptions, CreateContainerOptions, CreateImageOptions, ListContainersOptions,
    LogsOptions, RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
    WaitContainerOptions,
};
use futures_util::StreamExt;
use std::collections::HashMap;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

/// Everything needed to run one throwaway helper container to completion
/// via [`DockerClient::run_to_completion`].
pub struct ContainerRunSpec {
    /// Image reference to run.
    pub image: String,
    /// Command argv. Empty leaves the image's default command in place.
    pub cmd: Vec<String>,
    /// `KEY=VALUE` environment entries.
    pub env: Vec<String>,
    /// Bind mounts in `host:container[:mode]` form.
    pub binds: Vec<String>,
    /// User network to join (`--network`), if any.
    pub network: Option<String>,
    /// `--add-host` entries in `host:ip` form.
    pub extra_hosts: Vec<String>,
    /// Working directory inside the container.
    pub working_dir: Option<String>,
    /// Overrides the image entrypoint when set.
    pub entrypoint: Option<Vec<String>>,
    /// Fed to the container's stdin, after which stdin is closed. `None`
    /// leaves stdin unattached.
    pub stdin: Option<String>,
    /// Pull the image before running. Skip when it's already local.
    pub pull: bool,
}

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

    /// Create and start an anonymous container, stream its logs, wait for it
    /// to exit, remove it, and return its exit code. The container is removed
    /// even on the error paths.
    pub async fn run_to_completion(&self, label: &str, spec: ContainerRunSpec) -> Result<i64> {
        if spec.pull {
            self.pull_image(&spec.image).await?;
        }

        let host_config = HostConfig {
            binds: Some(spec.binds),
            network_mode: spec.network,
            extra_hosts: if spec.extra_hosts.is_empty() {
                None
            } else {
                Some(spec.extra_hosts)
            },
            ..Default::default()
        };
        let want_stdin = spec.stdin.is_some();
        let config = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: Some(spec.cmd),
            env: Some(spec.env),
            entrypoint: spec.entrypoint,
            working_dir: spec.working_dir,
            open_stdin: Some(want_stdin),
            attach_stdin: Some(want_stdin),
            stdin_once: Some(want_stdin),
            host_config: Some(host_config),
            ..Default::default()
        };

        // Anonymous name (None) avoids collisions across runs.
        let opts = CreateContainerOptions {
            name: None,
            platform: String::new(),
        };
        let id = self
            .client
            .create_container(Some(opts), config)
            .await
            .with_context(|| format!("{label}: create {}", spec.image))?
            .id;

        // Run the body separately so we always remove the container after.
        let result = self.run_body(&id, label, spec.stdin).await;

        let _ = self
            .client
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        result
    }

    async fn run_body(&self, id: &str, label: &str, stdin: Option<String>) -> Result<i64> {
        // Attach and push stdin (only when requested) before starting.
        if let Some(input) = stdin {
            let mut io = self
                .client
                .attach_container(
                    id,
                    Some(AttachContainerOptions {
                        stdin: true,
                        stream: true,
                        ..Default::default()
                    }),
                )
                .await
                .with_context(|| format!("{label}: attach stdin"))?;
            self.client
                .start_container(id, None::<StartContainerOptions>)
                .await
                .with_context(|| format!("{label}: start"))?;
            io.input.write_all(input.as_bytes()).await.ok();
            io.input.shutdown().await.ok();
        } else {
            self.client
                .start_container(id, None::<StartContainerOptions>)
                .await
                .with_context(|| format!("{label}: start"))?;
        }

        // Stream logs (blocks until the container stops).
        let mut logs = self.client.logs(
            id,
            Some(LogsOptions {
                follow: true,
                stdout: true,
                stderr: true,
                ..Default::default()
            }),
        );
        while let Some(chunk) = logs.next().await {
            if let Ok(out) = chunk {
                let line = strip_ansi(&String::from_utf8_lossy(&out.into_bytes()))
                    .trim_end()
                    .to_string();
                if !line.is_empty() {
                    info!("[{label}] {line}");
                }
            }
        }

        // Exit code. A non-zero exit surfaces as DockerContainerWaitError.
        let mut wait = self.client.wait_container(id, None::<WaitContainerOptions>);
        let mut code = 0i64;
        while let Some(res) = wait.next().await {
            match res {
                Ok(r) => code = r.status_code,
                Err(bollard::errors::Error::DockerContainerWaitError { code: c, .. }) => code = c,
                Err(e) => return Err(e).with_context(|| format!("{label}: wait")),
            }
        }
        Ok(code)
    }
}

/// Remove ANSI escape sequences (SGR color codes and other CSI/escape forms)
/// from helper container output. Some tools colorize their logs even to a
/// non-TTY pipe; without this the re-logged lines carry raw `\x1b[..m` bytes.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if let Some('[') = chars.next() {
            for f in chars.by_ref() {
                // CSI sequence: ESC + '[' + ... + final byte in 0x40..=0x7e
                if ('\x40'..='\x7e').contains(&f) {
                    break;
                }
            }
        }
    }
    out
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
    use super::strip_ansi;

    #[test]
    fn strip_ansi_removes_sgr_codes() {
        let input = "\x1b[2m2026-08-03T14:34:32Z\x1b[0m \x1b[32m INFO\x1b[0m imported";
        assert_eq!(strip_ansi(input), "2026-08-03T14:34:32Z  INFO imported");
    }

    #[test]
    fn strip_ansi_leaves_plain_text_untouched() {
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }

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
