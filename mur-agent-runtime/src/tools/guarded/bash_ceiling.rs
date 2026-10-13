//! #1814: record each `bash` call the bash tier ceiling turned from `allow`
//! into an approval.
//!
//! #1833 made the ceiling enforce with no observation period first: every
//! `bash: allow` agent starts asking for `git push`, `curl`, `rm` the moment
//! it lands. With nobody at the screen a stopped call does not run (a fleet
//! run defers it, a `channel/delegate` turn refuses it) and shows nothing on
//! screen. This record is how `mur agent doctor` shows which agents the
//! ceiling stops, how often, and on what — `enforced: true`. A call a
//! remembered decision answers is not a stop and is not recorded.

use mur_common::agent::ToolPolicy;
use mur_common::hitl::floor_waiver::{CeilingHit, bash_ceiling_hit};
use mur_common::telemetry::METHOD_BASH_CEILING_EXCEEDED;

use super::GuardedToolCall;
use crate::hitl::batch::PendingCall;
use crate::llm::ToolCallResult;
use crate::task_runner::effective_tool_policy;
use crate::telemetry_writer::Event;

/// Same cap as the turn ledger's `target`, so a record lines up with the
/// ledger row for the same call.
const COMMAND_PREVIEW_CHARS: usize = 400;

impl GuardedToolCall {
    /// Called for a call the gate stops: about to ask a human, or refusing
    /// it outright. Never for one a remembered decision answers — nobody was
    /// stopped. Records only when the CEILING is why: a call whose rule
    /// already asks (`bash: ask`, a declared `risk:`, no rule at all) would
    /// have asked without it.
    pub(crate) async fn record_bash_ceiling(&self, task_id: &str, call: &ToolCallResult) {
        if effective_tool_policy(&self.tools_policy, &call.tool_name) != ToolPolicy::Allow {
            return;
        }
        let Some(hit) = bash_ceiling_hit(&self.tools_policy, &call.tool_name, Some(&call.input))
        else {
            return;
        };
        let command = self.masked(command_preview(&call.input));
        tracing::info!(
            agent = %self.agent_name,
            tool = %call.tool_name,
            tier = ?hit.tier,
            ceiling = ?hit.ceiling,
            rule = hit.rule.as_deref().unwrap_or(""),
            command = %command,
            "bash call above the bash tier ceiling needs approval (#1814)"
        );
        if let Some(tx) = &self.telemetry {
            // Best-effort like every telemetry send: never blocks the call.
            let _ = tx.try_send(Event::HookFired {
                method: METHOD_BASH_CEILING_EXCEEDED.to_string(),
                attrs: record(&self.agent_name, task_id, call, &command, &hit),
            });
        }
    }

    /// [`Self::record_bash_ceiling`] for a call already turned into a
    /// [`PendingCall`]: records the original call it came from.
    pub(crate) async fn record_pending_ceiling(
        &self,
        task_id: &str,
        calls: &[ToolCallResult],
        pending: &PendingCall,
    ) {
        if let Some(call) = calls.iter().find(|c| c.call_id == pending.call_id) {
            self.record_bash_ceiling(task_id, call).await;
        }
    }
}

fn command_preview(input: &serde_json::Value) -> String {
    let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
    match cmd.char_indices().nth(COMMAND_PREVIEW_CHARS) {
        Some((i, _)) => format!("{}…", &cmd[..i]),
        None => cmd.to_string(),
    }
}

fn record(
    agent: &str,
    task_id: &str,
    call: &ToolCallResult,
    command: &str,
    hit: &CeilingHit,
) -> serde_json::Value {
    // `HookFired` skips the writer's `mur.event.type` stamp, so the record
    // carries its own — `mur agent doctor` finds these lines by it.
    serde_json::json!({
        mur_common::telemetry::MUR_EVENT_TYPE: METHOD_BASH_CEILING_EXCEEDED,
        mur_common::telemetry::MUR_AGENT_NAME: agent,
        mur_common::telemetry::MUR_TASK_ID: task_id,
        "call_id": call.call_id,
        "tool": call.tool_name,
        "command": command,
        "tier": hit.tier,
        "ceiling": hit.ceiling,
        "rule": hit.rule,
        "enforced": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_caps_on_a_char_boundary() {
        let long = "é".repeat(COMMAND_PREVIEW_CHARS + 5);
        let p = command_preview(&serde_json::json!({ "command": long }));
        assert_eq!(p.chars().count(), COMMAND_PREVIEW_CHARS + 1);
        assert!(p.ends_with('…'));
        let short = command_preview(&serde_json::json!({ "command": "git push" }));
        assert_eq!(short, "git push");
    }
}
