//! `shell`: run an arbitrary command. The escape hatch for anything
//! not covered by a dedicated step kind. The shell interpreter
//! (`cmd`, `sh`, `pwsh`, `powershell`) is selectable per-step and
//! defaults to the platform-native choice.

use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::process::Command;
use tracing::info;

pub struct Shell {
    command: String,
    shell: ShellKind,
}

/// Which shell interpreter to invoke for the `command`.
///
/// Deserialized from the step's `shell` field. When omitted, the step
/// falls back to [`ShellKind::default_for_platform`] (`cmd` on Windows,
/// `sh` elsewhere).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    /// Windows `cmd.exe`, invoked as `cmd /C <command>`.
    Cmd,
    /// POSIX `sh`, invoked as `sh -c <command>`.
    Sh,
    /// PowerShell 7+ (`pwsh`), invoked with
    /// `-NoProfile -NonInteractive -Command <command>`.
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell`), same flags as [`Self::Pwsh`].
    /// Selected via the TOML value `"powershell"`.
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
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let raw: ShellRaw = body.try_into().context("invalid shell config")?;
        Ok(Self {
            command: raw.command,
            shell: raw.shell.unwrap_or_else(ShellKind::default_for_platform),
        })
    }
}

impl Step for Shell {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let program = self.shell.program();
            info!("[{}] running via {}: {}", id, program, self.command);
            let status = Command::new(program)
                .args(self.shell.args(&self.command))
                .status()
                .await
                .with_context(|| format!("step '{}': failed to spawn {}", id, program))?;

            if !status.success() {
                bail!("step '{}': command exited with {}", id, status);
            }
            Ok(StepOutcome::default())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_distinct() {
        assert_eq!(ShellKind::Cmd.program(), "cmd");
        assert_eq!(ShellKind::Sh.program(), "sh");
        assert_eq!(ShellKind::Pwsh.program(), "pwsh");
        assert_eq!(ShellKind::WinPwsh.program(), "powershell");
    }

    #[test]
    fn cmd_uses_slash_c() {
        assert_eq!(ShellKind::Cmd.args("echo hi"), vec!["/C", "echo hi"]);
    }

    #[test]
    fn sh_uses_dash_c() {
        assert_eq!(ShellKind::Sh.args("echo hi"), vec!["-c", "echo hi"]);
    }

    #[test]
    fn pwsh_uses_command_flag() {
        assert_eq!(
            ShellKind::Pwsh.args("Write-Host hi"),
            vec!["-NoProfile", "-NonInteractive", "-Command", "Write-Host hi"]
        );
    }

    #[test]
    fn winpwsh_shares_pwsh_flags() {
        assert_eq!(
            ShellKind::WinPwsh.args("Write-Host hi"),
            vec!["-NoProfile", "-NonInteractive", "-Command", "Write-Host hi"]
        );
    }

    #[test]
    fn platform_default_is_cmd_on_windows_sh_elsewhere() {
        let d = ShellKind::default_for_platform();
        if cfg!(target_os = "windows") {
            assert!(matches!(d, ShellKind::Cmd));
        } else {
            assert!(matches!(d, ShellKind::Sh));
        }
    }
}
