//! `container_run`: run a one-shot helper container to completion via the
//! Docker API.
//!
//! The generic way to use a component-specific tool (`unzip`, a DB client,
//! `rsync`, ...) without baking it into the archanist image. The engine
//! creates the helper container through the same bollard socket
//! `docker_swap` uses, so recipes only name the tool image; the archanist
//! image needs nothing but its own binary and the mounted docker socket.
//!
//! A non-zero container exit fails the step.

use crate::docker::{ContainerRunSpec, DockerClient};
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ContainerRunRaw {
    /// Image to run. Required and non-empty.
    image: String,
    /// Command argv. Empty leaves the image default in place.
    #[serde(default)]
    command: Vec<String>,
    /// `KEY=VALUE` environment entries.
    #[serde(default)]
    env: Vec<String>,
    /// Bind mounts in `host:container[:mode]` form.
    #[serde(default)]
    binds: Vec<String>,
    /// User network to join (`--network`).
    #[serde(default)]
    network: Option<String>,
    /// `--add-host` entries in `host:ip` form.
    #[serde(default)]
    extra_hosts: Vec<String>,
    /// Working directory inside the container.
    #[serde(default)]
    workdir: Option<String>,
    /// Overrides the image entrypoint when set.
    #[serde(default)]
    entrypoint: Option<Vec<String>>,
    /// Text fed to the container's stdin, after which stdin is closed.
    #[serde(default)]
    stdin: Option<String>,
    /// Pull the image before running. Defaults to `true`.
    #[serde(default = "default_pull")]
    pull: bool,
}

fn default_pull() -> bool {
    true
}

#[derive(Debug)]
pub struct ContainerRun {
    raw: ContainerRunRaw,
}

impl ContainerRun {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: ContainerRunRaw = body.try_into().context("invalid container_run config")?;
        if raw.image.trim().is_empty() {
            bail!("`image` must not be empty");
        }
        Ok(Self { raw })
    }
}

impl Step for ContainerRun {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let docker = DockerClient::connect()?;
            let code = docker
                .run_to_completion(
                    id,
                    ContainerRunSpec {
                        image: self.raw.image.clone(),
                        cmd: self.raw.command.clone(),
                        env: self.raw.env.clone(),
                        binds: self.raw.binds.clone(),
                        network: self.raw.network.clone(),
                        extra_hosts: self.raw.extra_hosts.clone(),
                        working_dir: self.raw.workdir.clone(),
                        entrypoint: self.raw.entrypoint.clone(),
                        stdin: self.raw.stdin.clone(),
                        pull: self.raw.pull,
                    },
                )
                .await?;
            if code != 0 {
                bail!("step '{}': container exited with {}", id, code);
            }
            Ok(StepOutcome::default())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &str) -> toml::Value {
        toml::Value::Table(toml::from_str(text).unwrap())
    }

    #[test]
    fn from_body_requires_non_empty_image() {
        let err = ContainerRun::from_body(body(r#"image = "  ""#)).expect_err("expected error");
        assert!(err.to_string().contains("must not be empty"), "err: {err}");
    }

    #[test]
    fn from_body_defaults_pull_to_true() {
        let step = ContainerRun::from_body(body(r#"image = "busybox""#)).unwrap();
        assert!(step.raw.pull);
    }

    #[test]
    fn from_body_reads_all_fields() {
        let step = ContainerRun::from_body(body(
            r#"
                image = "busybox"
                command = ["unzip", "-o", "a.zip"]
                binds = ["/host:/app/data"]
                network = "mynet"
                extra_hosts = ["db:host-gateway"]
                workdir = "/app/data"
                stdin = "hello"
                pull = false
            "#,
        ))
        .unwrap();
        assert_eq!(step.raw.command, vec!["unzip", "-o", "a.zip"]);
        assert_eq!(step.raw.binds, vec!["/host:/app/data"]);
        assert_eq!(step.raw.network.as_deref(), Some("mynet"));
        assert_eq!(step.raw.extra_hosts, vec!["db:host-gateway"]);
        assert_eq!(step.raw.workdir.as_deref(), Some("/app/data"));
        assert_eq!(step.raw.stdin.as_deref(), Some("hello"));
        assert!(!step.raw.pull);
    }
}
