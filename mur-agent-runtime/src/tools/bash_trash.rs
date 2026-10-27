//! `rm` → trash for the `bash` tool (issue #2154, spec D1 / D9 / D10).
//!
//! `BashTool::execute` asks [`intercept`] before spawning anything. A command
//! made only of removers MUR fully understands never reaches a shell: its
//! targets are moved into the agent's trash and the model gets an `rm`-like
//! reply. Anything else runs as before; a remover MUR could not rewrite is
//! recorded as an unprotected delete first.

use std::path::{Path, PathBuf};

use mur_common::action::DeletionConfig;
use mur_trash::{RmPlan, TrashOutcome};
use uuid::Uuid;

use super::{ToolOutput, ToolStatus};

/// Exit code reported when any target was not deleted, as `rm` does.
const RM_FAILURE_EXIT: i32 = 1;

/// Where an agent's trash lives and how big it may grow. Present on a
/// `BashTool` only when `hooks.trash_guard` is on (D10).
#[derive(Debug, Clone)]
pub struct BashTrash {
    /// The agent's home: trash and ledger live under it.
    pub agent_home: PathBuf,
    /// Canonical agent name, for the restore command shown to the model.
    pub agent_name: String,
    pub deletion: DeletionConfig,
}

/// `Some(reply)` when the command was handled here and must NOT be run.
pub(crate) async fn intercept(
    trash: &BashTrash,
    command: &str,
    working_dir: &Path,
) -> Option<ToolOutput> {
    match mur_trash::plan(command, working_dir) {
        RmPlan::NotRm => None,
        RmPlan::Rewrite(targets) => {
            let trash = trash.clone();
            let task_id = current_task_uuid();
            let outcomes = tokio::task::spawn_blocking(move || {
                mur_trash::trash_into(&trash.agent_home, &targets, &trash.deletion, task_id)
            })
            .await;
            match outcomes {
                Ok(outcomes) => Some(render(&outcomes, &trash.agent_name)),
                // A panic inside the mover: nothing was reported, so say so
                // rather than fall through and let the real `rm` run.
                Err(e) => Some(ToolOutput {
                    text: format!("rm: trash move aborted: {e}\n[exit code: {RM_FAILURE_EXIT}]"),
                    status: ToolStatus::Failed {
                        exit_code: RM_FAILURE_EXIT,
                    },
                    images: Vec::new(),
                }),
            }
        }
        RmPlan::Unrewritable { reason } => {
            record(trash, command, &reason).await;
            None
        }
        RmPlan::Unparseable => {
            record(trash, command, "unparseable").await;
            None
        }
    }
}

/// D7 audit line. The command runs whether or not this lands.
async fn record(trash: &BashTrash, command: &str, reason: &str) {
    let home = trash.agent_home.clone();
    let (command, reason) = (command.to_owned(), reason.to_owned());
    let res = tokio::task::spawn_blocking(move || {
        mur_trash::record_unprotected(&home, &command, &reason)
    })
    .await;
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "could not record unprotected delete"),
        Err(e) => tracing::warn!(error = %e, "unprotected-delete recorder panicked"),
    }
}

/// The turn's task id, or nil outside a turn (D8: `created_by_task_id`).
fn current_task_uuid() -> Uuid {
    super::bash_jobs::current_task_id()
        .and_then(|s| Uuid::parse_str(&s).ok())
        .unwrap_or(Uuid::nil())
}

/// D9: one line per target; exit 1 if any target was not deleted.
pub(crate) fn render(outcomes: &[TrashOutcome], agent: &str) -> ToolOutput {
    let mut lines = Vec::new();
    for o in outcomes {
        match o {
            TrashOutcome::Trashed {
                target, entry_id, ..
            } => lines.push(format!(
                "moved to trash: {} (restore: mur agent trash {agent} restore {entry_id})",
                target.path.display()
            )),
            TrashOutcome::CopiedSourceKept {
                target,
                entry_id,
                error,
            } => lines.push(format!(
                "rm: {}: copied to trash as {entry_id} but the original could not be removed: {error}",
                target.spelled
            )),
            TrashOutcome::Evicted { target, cap_mb } => lines.push(format!(
                "deleted permanently (trash is over its {cap_mb} MB cap): {}",
                target.path.display()
            )),
            TrashOutcome::MissingIgnored { .. } => {}
            TrashOutcome::Failed { target, error } => {
                lines.push(format!("rm: {}: {error}", target.spelled))
            }
        }
    }
    let mut status = ToolStatus::Ok;
    if outcomes.iter().any(TrashOutcome::is_failure) {
        lines.push(format!("[exit code: {RM_FAILURE_EXIT}]"));
        status = ToolStatus::Failed {
            exit_code: RM_FAILURE_EXIT,
        };
    }
    ToolOutput {
        text: lines.join("\n"),
        status,
        images: Vec::new(),
    }
}

#[cfg(all(test, unix))]
#[path = "bash_trash_tests.rs"]
mod tests;
