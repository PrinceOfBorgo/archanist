use crate::config::StepConfig;
use crate::steps::Step;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::Command;
use tracing::info;

pub struct Shell {
    id: String,
    command: String,
    shell: ShellKind,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    Cmd,
    Sh,
    Pwsh,
    #[serde(rename = "powershell")]
    WinPwsh,
}

impl ShellKind {
    fn default_for_platform() -> Self {
        if cfg!(target_os = "windows") {
            Self::Cmd
        } else {
            Self::Sh
        }
    }

    fn program(&self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Sh => "sh",
            Self::Pwsh => "pwsh",
            Self::WinPwsh => "powershell",
        }
    }

    fn args<'a>(&self, command: &'a str) -> Vec<&'a str> {
        match self {
            Self::Cmd => vec!["/C", command],
            Self::Sh => vec!["-c", command],
            Self::Pwsh | Self::WinPwsh => {
                vec!["-NoProfile", "-NonInteractive", "-Command", command]
            }
        }
    }
}

#[derive(Deserialize)]
struct ShellRaw {
    command: String,
    #[serde(default)]
    shell: Option<ShellKind>,
}

impl Shell {
    pub fn from_config(cfg: &StepConfig) -> Result<Self> {
        let raw: ShellRaw = toml::Value::Table(cfg.extra.clone())
            .try_into()
            .with_context(|| format!("step '{}': invalid shell config", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            command: raw.command,
            shell: raw.shell.unwrap_or_else(ShellKind::default_for_platform),
        })
    }
}

impl Step for Shell {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &str {
        "shell"
    }

    fn describe(&self) -> String {
        format!("run ({}): {}", self.shell.program(), self.command)
    }

    fn execute(&self) -> Result<()> {
        let program = self.shell.program();
        info!("[{}] running via {}: {}", self.id, program, self.command);
        let status = Command::new(program)
            .args(self.shell.args(&self.command))
            .status()
            .with_context(|| format!("step '{}': failed to spawn {}", self.id, program))?;

        if !status.success() {
            bail!("step '{}': command exited with {}", self.id, status);
        }
        Ok(())
    }
}
