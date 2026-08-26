//! Persistent state for the archanist.
//!
//! Tracks per-component:
//! - the currently-installed version and its predecessor,
//! - the last release check (timestamp and version seen),
//! - the most recent update attempt with all its steps (used by
//!   rollback and by resumable / self-updating pipelines), and
//! - a `blocklist` of versions the operator has told us to skip.
//!
//! The whole state is round-tripped as TOML through [`ArchanistState::load`]
//! and [`ArchanistState::save`].

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ArchanistState {
    #[serde(default)]
    pub components: HashMap<String, ComponentState>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ComponentState {
    pub current_version: Option<String>,
    pub previous_version: Option<String>,
    pub latest_check_version: Option<String>,
    pub last_check: Option<DateTime<Utc>>,
    /// The most recent update attempt - succeeded, failed, or in-flight.
    /// Rollback consults the payload snapshots stored inside its steps.
    #[serde(default)]
    pub last_attempt: Option<UpdateAttempt>,
    #[serde(default)]
    pub blocklist: Vec<String>,
}

/// One end-to-end update run for a component. Recorded when the update
/// starts (with `outcome = InFlight`), grown as each step finishes, and
/// finalized when the pipeline exits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAttempt {
    /// Version the pipeline is upgrading to.
    pub target_version: String,
    /// Version installed when the attempt started, if any. Stays in
    /// sync with [`ComponentState::current_version`] at the moment
    /// the attempt began.
    pub current_version: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub outcome: AttemptOutcome,
    /// Per-step records in pipeline order. Populated by
    /// [`UpdateAttempt::mark_step_done`] / friends.
    pub steps: Vec<StepRun>,
}

/// Terminal (or in-flight) status of an [`UpdateAttempt`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// Pipeline is still running (we crashed, were killed, or a
    /// self-update is waiting for the archanist container to restart).
    InFlight,
    /// All steps applied successfully.
    Success,
    /// At least one step failed. `message` holds the error summary.
    Failed { message: String },
}

/// One step's contribution to an [`UpdateAttempt`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRun {
    pub id: String,
    pub kind: String,
    pub state: StepRunState,
    /// Opaque payload written by the step's `apply` and read by
    /// its `rollback`.
    #[serde(default = "empty_payload")]
    pub payload: toml::Value,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepRunState {
    /// Recorded before any step in the attempt runs.
    Pending,
    /// Step is currently executing.
    Running,
    /// Step's `apply` returned successfully.
    Done,
    /// Step's `apply` returned an error.
    Failed,
    /// Step applied successfully but its effects were later undone by
    /// `archanist rollback`.
    RolledBack,
}

fn empty_payload() -> toml::Value {
    toml::Value::Table(Default::default())
}

impl UpdateAttempt {
    /// Start a new attempt for `target_version`, pre-populating step
    /// slots (all [`StepRunState::Pending`]) so the update loop can
    /// address them by index.
    pub fn start(
        target_version: String,
        current_version: Option<String>,
        step_ids_and_kinds: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let steps = step_ids_and_kinds
            .into_iter()
            .map(|(id, kind)| StepRun {
                id,
                kind,
                state: StepRunState::Pending,
                payload: empty_payload(),
                started_at: None,
                finished_at: None,
                message: None,
            })
            .collect();
        Self {
            target_version,
            current_version,
            started_at: Utc::now(),
            finished_at: None,
            outcome: AttemptOutcome::InFlight,
            steps,
        }
    }

    fn slot_mut(&mut self, id: &str) -> Option<&mut StepRun> {
        self.steps.iter_mut().find(|s| s.id == id)
    }

    /// Move a step to [`StepRunState::Running`] and stamp `started_at`.
    /// Returns `false` if `id` is unknown to the attempt (shouldn't
    /// happen if the attempt was built from the same pipeline).
    pub fn mark_step_running(&mut self, id: &str) -> bool {
        if let Some(slot) = self.slot_mut(id) {
            slot.state = StepRunState::Running;
            slot.started_at = Some(Utc::now());
            true
        } else {
            false
        }
    }

    /// Record a successful step: state = [`StepRunState::Done`],
    /// persist `payload`, stamp `finished_at`.
    pub fn mark_step_done(&mut self, id: &str, payload: toml::Value) -> bool {
        if let Some(slot) = self.slot_mut(id) {
            slot.state = StepRunState::Done;
            slot.payload = payload;
            slot.finished_at = Some(Utc::now());
            true
        } else {
            false
        }
    }

    /// Record a failed step and finalize the whole attempt as
    /// [`AttemptOutcome::Failed`].
    pub fn mark_step_failed(&mut self, id: &str, message: String) {
        if let Some(slot) = self.slot_mut(id) {
            slot.state = StepRunState::Failed;
            slot.finished_at = Some(Utc::now());
            slot.message = Some(message.clone());
        }
        self.finished_at = Some(Utc::now());
        self.outcome = AttemptOutcome::Failed { message };
    }

    /// Mark the whole attempt successful.
    pub fn mark_success(&mut self) {
        self.finished_at = Some(Utc::now());
        self.outcome = AttemptOutcome::Success;
    }
}

impl ArchanistState {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            debug!("state file {} not found, starting fresh", path.display());
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read state file: {}", path.display()))?;
        let state: Self = toml::from_str(&content).context("Failed to parse state file")?;
        debug!(
            "loaded state from {} ({} component(s))",
            path.display(),
            state.components.len()
        );
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("Failed to serialize state to TOML")?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, &content)
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| {
            format!(
                "Failed to move {} into place at {}",
                tmp.display(),
                path.display()
            )
        })?;
        debug!("saved state to {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt_with_two_steps() -> UpdateAttempt {
        UpdateAttempt::start(
            "1.2.3".into(),
            Some("1.2.2".into()),
            [
                ("download".to_string(), "download".to_string()),
                ("swap".to_string(), "docker_swap".to_string()),
            ],
        )
    }

    #[test]
    fn start_populates_pending_slots_in_order() {
        let a = attempt_with_two_steps();
        assert_eq!(a.outcome, AttemptOutcome::InFlight);
        assert_eq!(a.steps.len(), 2);
        assert_eq!(a.steps[0].id, "download");
        assert_eq!(a.steps[0].state, StepRunState::Pending);
        assert_eq!(a.steps[1].id, "swap");
        assert_eq!(a.steps[1].state, StepRunState::Pending);
    }

    #[test]
    fn mark_step_running_then_done_updates_slot_and_payload() {
        let mut a = attempt_with_two_steps();
        assert!(a.mark_step_running("download"));
        assert_eq!(a.steps[0].state, StepRunState::Running);
        assert!(a.steps[0].started_at.is_some());

        let payload = toml::from_str::<toml::Value>("dest = \"/tmp/x\"\n").unwrap();
        assert!(a.mark_step_done("download", payload));
        assert_eq!(a.steps[0].state, StepRunState::Done);
        assert_eq!(
            a.steps[0].payload.get("dest").and_then(|v| v.as_str()),
            Some("/tmp/x")
        );
        assert!(a.steps[0].finished_at.is_some());
    }

    #[test]
    fn mark_step_failed_finalizes_attempt() {
        let mut a = attempt_with_two_steps();
        a.mark_step_running("swap");
        a.mark_step_failed("swap", "docker refused".into());
        assert_eq!(a.steps[1].state, StepRunState::Failed);
        assert_eq!(a.steps[1].message.as_deref(), Some("docker refused"));
        assert!(a.finished_at.is_some());
        assert_eq!(
            a.outcome,
            AttemptOutcome::Failed {
                message: "docker refused".into()
            }
        );
    }

    #[test]
    fn mark_success_finalizes_attempt() {
        let mut a = attempt_with_two_steps();
        a.mark_success();
        assert_eq!(a.outcome, AttemptOutcome::Success);
        assert!(a.finished_at.is_some());
    }

    #[test]
    fn mark_unknown_id_is_a_noop() {
        let mut a = attempt_with_two_steps();
        assert!(!a.mark_step_running("nope"));
        assert!(!a.mark_step_done("nope", empty_payload()));
    }

    #[test]
    fn state_with_last_attempt_roundtrips() {
        let mut state = ArchanistState::default();
        let mut attempt = attempt_with_two_steps();
        attempt.mark_step_running("download");
        attempt.mark_step_done(
            "download",
            toml::from_str::<toml::Value>("dest = \"/tmp/x\"\n").unwrap(),
        );
        attempt.mark_step_running("swap");
        attempt.mark_step_done(
            "swap",
            toml::from_str::<toml::Value>("previous_image = \"nginx:1.24\"\n").unwrap(),
        );
        attempt.mark_success();

        let comp = ComponentState {
            current_version: Some("1.2.3".into()),
            last_attempt: Some(attempt),
            ..Default::default()
        };
        state.components.insert("web".into(), comp);
        let s = toml::to_string(&state).unwrap();
        let back: ArchanistState = toml::from_str(&s).unwrap();
        let a = back.components["web"]
            .last_attempt
            .as_ref()
            .expect("last_attempt");
        assert_eq!(a.outcome, AttemptOutcome::Success);
        assert_eq!(a.steps.len(), 2);
        assert_eq!(
            a.steps[1]
                .payload
                .get("previous_image")
                .and_then(|v| v.as_str()),
            Some("nginx:1.24")
        );
    }

    #[test]
    fn state_with_blocklist_roundtrips() {
        let mut state = ArchanistState::default();
        let comp = ComponentState {
            current_version: Some("1.0.0".into()),
            blocklist: vec!["1.0.5".into(), "1.0.6-alpha".into()],
            ..Default::default()
        };
        state.components.insert("web".into(), comp);
        let s = toml::to_string(&state).unwrap();
        let back: ArchanistState = toml::from_str(&s).unwrap();
        assert_eq!(back.components["web"].blocklist.len(), 2);
        assert!(back.components["web"].blocklist.contains(&"1.0.5".into()));
    }

    #[test]
    fn blocklist_and_last_attempt_default_when_absent() {
        // Older state files without the new fields should still deserialize.
        let s = r#"
[components.web]
current_version = "1.0.0"
"#;
        let back: ArchanistState = toml::from_str(s).unwrap();
        assert!(back.components["web"].blocklist.is_empty());
        assert!(back.components["web"].last_attempt.is_none());
    }
}
