//! Hub side of the `propose` tool (#1566): turn a streamed `step/started` for
//! `propose` into a chip above the chat composer.
//!
//! The Hub composer has no `!` shell or `/` slash mode — everything typed is
//! sent to the agent as chat. So unlike murmur, shell/slash chips here never
//! insert into the composer (Enter would send the command as a message); the
//! frontend offers **copy** instead. Only `restart` is actionable, behind an
//! explicit confirm, via [`restart_agent`].
//!
//! Same gate as murmur and the runtime executor: [`vet`] re-runs here, so a
//! proposal the model was told was rejected is never shown.

use mur_common::proposal::{KIND_RESTART, KIND_SHELL, KIND_SLASH, PROPOSE_TOOL, ProposalKind, vet};
use mur_core::a2a_dial::StepEvent;
use serde::Serialize;
use tauri::State;

use crate::SupervisorState;

/// Tauri event carrying a [`ChipPayload`] to the chat view.
pub const PROPOSAL_EVENT: &str = "proposal-offered";

/// What the chat view renders. One chip per agent; a newer one replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChipPayload {
    /// Proposing agent — the chat view filters on this.
    pub agent: String,
    pub label: String,
    /// `shell` / `slash` / `restart` (the tool's own kind strings).
    pub kind: &'static str,
    /// Text to show and copy: bare shell command, or `/cmd` for slash.
    /// `None` for `restart`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Map one streamed step to a chip, or `None` when it is not a vetted
/// `propose` call. Rejections are logged (with the agent and reason) so a
/// missing chip is debuggable rather than silent.
pub fn chip_from_step(agent: &str, step: &StepEvent) -> Option<ChipPayload> {
    let StepEvent::Started {
        name,
        args,
        step_id,
        ..
    } = step
    else {
        return None;
    };
    if name != PROPOSE_TOOL {
        return None;
    }
    let p = match vet(args) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(agent, step_id, "propose rejected, no chip: {e}");
            return None;
        }
    };
    let (kind, command) = match p.kind {
        ProposalKind::Shell(cmd) => (KIND_SHELL, Some(cmd)),
        ProposalKind::Slash(cmd) => (KIND_SLASH, Some(format!("/{cmd}"))),
        ProposalKind::Restart => (KIND_RESTART, None),
        // `vet` never builds a reply; murmur's ghost is not a Hub chip.
        ProposalKind::Reply(_) => return None,
    };
    Some(ChipPayload {
        agent: agent.to_string(),
        label: p.label,
        kind,
        command,
    })
}

/// Restart one agent for an accepted `restart` chip: stop, then start.
/// The supervisor actor handles messages in order and `Stop` waits for the
/// child to exit, so the `Start` never races a still-running runtime.
#[tauri::command]
pub async fn restart_agent(
    name: String,
    supervisor: State<'_, SupervisorState>,
) -> Result<(), String> {
    supervisor.0.stop(&name).await;
    supervisor.0.start(&name).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn started(name: &str, args: Value) -> StepEvent {
        StepEvent::Started {
            step_id: "s-1".into(),
            task_id: "t-1".into(),
            name: name.into(),
            args,
        }
    }

    #[test]
    fn shell_proposal_becomes_bare_command_chip() {
        let chip = chip_from_step(
            "mur",
            &started(
                PROPOSE_TOOL,
                json!({"label": "open items", "kind": "shell", "command": "!mur open"}),
            ),
        )
        .expect("vetted shell chip");
        assert_eq!(chip.agent, "mur");
        assert_eq!(chip.kind, KIND_SHELL);
        assert_eq!(chip.command.as_deref(), Some("mur open"));
    }

    #[test]
    fn slash_proposal_keeps_its_slash() {
        let chip = chip_from_step(
            "mur",
            &started(
                PROPOSE_TOOL,
                json!({"label": "model", "kind": "slash", "command": "model"}),
            ),
        )
        .unwrap();
        assert_eq!(chip.command.as_deref(), Some("/model"));
    }

    #[test]
    fn restart_proposal_has_no_command() {
        let chip = chip_from_step(
            "mur",
            &started(PROPOSE_TOOL, json!({"label": "apply", "kind": "restart"})),
        )
        .unwrap();
        assert_eq!(chip.kind, KIND_RESTART);
        assert_eq!(chip.command, None);
        // The frontend keys off absence, so it must not serialize as null.
        assert!(
            serde_json::to_value(&chip)
                .unwrap()
                .get("command")
                .is_none()
        );
    }

    #[test]
    fn rejected_proposal_emits_nothing() {
        let placeholder =
            json!({"label": "x", "kind": "shell", "command": "mur agent send <name>"});
        assert_eq!(
            chip_from_step("mur", &started(PROPOSE_TOOL, placeholder)),
            None
        );
        let restart_with_cmd = json!({"label": "x", "kind": "restart", "command": "mur"});
        assert_eq!(
            chip_from_step("mur", &started(PROPOSE_TOOL, restart_with_cmd)),
            None
        );
    }

    #[test]
    fn other_tools_and_completed_steps_are_ignored() {
        let args = json!({"label": "x", "kind": "restart"});
        assert_eq!(chip_from_step("mur", &started("bash", args)), None);
        let done = StepEvent::Completed {
            step_id: "s-1".into(),
            task_id: "t-1".into(),
            ok: true,
            output: String::new(),
            truncated: false,
            full_len: 0,
            error: None,
            duration_ms: 0,
            denied: false,
            running: false,
        };
        assert_eq!(chip_from_step("mur", &done), None);
    }
}
