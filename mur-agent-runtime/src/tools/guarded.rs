//! The guarded tool-call sequence: the one place a MUR tool runs.
//!
//! Every obligation here fails silently when it is skipped — a missing mask
//! leaks instead of erroring, a missing task scope orphans a `bash` job
//! instead of erroring. `task_runner.rs` carried two comments warning that a
//! third execution site would break exactly those two things. This type is
//! the answer: one owner, so "is the rule kept?" is a question about one
//! place instead of a question about vigilance.
//!
//! It exists to have a second caller — the MCP server that serves MUR's
//! tools to a spawned CLI — without that caller becoming a second path.
//! See `docs/superpowers/specs/2026-09-16-mur-tool-mcp-server-design.md`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::hitl::HitlApprovals;
use crate::task_runner::{
    cap_step_output, decide_without_asking, deny_message, effective_tool_policy, step_notification,
    task_error,
};
use mur_common::a2a::TaskError;

/// Everything the guarded sequence reads. Cloned from `TaskRunner` rather
/// than borrowed: the MCP server needs one that outlives any single turn's
/// borrow, and cloning `Arc`s and a small `Vec` per call is not a cost worth
/// a lifetime parameter here.
pub struct GuardedToolCall {
    pub(crate) tools: Vec<Arc<dyn crate::tools::ToolExecutor>>,
    pub(crate) tools_policy: Vec<mur_common::agent::ToolRule>,
    pub(crate) secrets: Option<Arc<crate::secrets::SecretVault>>,
    pub(crate) notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    pub(crate) client_notifiers:
        Arc<tokio::sync::Mutex<HashMap<String, crate::task_runner::ApprovalSink>>>,
    pub(crate) agent_name: String,
    pub(crate) decision_store: Option<Arc<dyn crate::hitl::store::DecisionStore>>,
    pub(crate) hitl_timeout_secs: u32,
    pub(crate) pending_approvals: Option<HitlApprovals>,
}

impl GuardedToolCall {
    /// D8: every tool executes inside the owner task's scope, from BOTH
    /// execute sites, so `bash` can stamp its job. One helper — a third site
    /// must call it too or its jobs belong to nobody.
    pub(crate) async fn execute_scoped(
        tool: &dyn crate::tools::ToolExecutor,
        task_id: &str,
        input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        crate::tools::bash_jobs::CURRENT_TASK_ID
            .scope(task_id.to_string(), tool.execute(input))
            .await
    }

    /// The one place tool output is scrubbed before it can reach the model.
    /// BOTH tool-execution sites call this; a third site must too, or that
    /// tool's output reaches the model unmasked.
    pub(crate) fn masked(&self, output: String) -> String {
        match &self.secrets {
            Some(v) => v.mask(&output).into_owned(),
            None => output,
        }
    }

    pub(crate) async fn gate_response(
        &self,
        task_id: &str,
        calls: &[crate::llm::ToolCallResult],
    ) -> (
        HashMap<String, crate::hitl::HitlDecision>,
        HashMap<String, String>,
    ) {
        use mur_common::agent::ToolPolicy;
        let known: std::collections::HashSet<String> =
            self.tools.iter().map(|t| t.name().to_string()).collect();
        let mut pending = Vec::new();
        let mut out = HashMap::new();
        let mut step_ids = HashMap::new();
        let entry = self.client_notifiers.lock().await.get(task_id).cloned();
        for call in calls {
            if !known.contains(&call.tool_name)
                || effective_tool_policy(&self.tools_policy, &call.tool_name) != ToolPolicy::Ask
            {
                continue;
            }
            if let Some(d) =
                decide_without_asking(entry.as_ref().map(|(_, ok)| *ok), &call.tool_name)
            {
                out.insert(call.call_id.clone(), d);
                continue;
            }
            let step_id = uuid::Uuid::now_v7().to_string();
            step_ids.insert(call.call_id.clone(), step_id.clone());
            pending.push(crate::hitl::batch::pending(&self.agent_name, step_id, call));
        }
        if pending.is_empty() {
            return (out, step_ids);
        }
        let routed = entry.map(|(tx, _)| tx);
        let notifier = routed.as_ref().or(self.notifier.as_ref());
        let (Some(pa), Some(notifier)) = (&self.pending_approvals, notifier) else {
            // fail-closed: no approval sink => deny.
            for c in pending {
                out.insert(
                    c.call_id,
                    crate::hitl::HitlDecision {
                        allow: false,
                        reason: Some("no approval channel available".into()),
                        surface: None,
                    },
                );
            }
            return (out, step_ids);
        };
        let gate = crate::hitl::batch::BatchGate {
            task_id,
            timeout: std::time::Duration::from_secs(self.hitl_timeout_secs as u64),
            approvals: pa,
            notifier,
            store: self.decision_store.as_ref(),
        };
        out.extend(gate.resolve(pending).await);
        (out, step_ids)
    }

    pub(crate) async fn handle_tool_call(
        &self,
        task_id: &str,
        call: &crate::llm::ToolCallResult,
        decision: Option<crate::hitl::HitlDecision>,
        step_id: Option<String>,
    ) -> Result<crate::llm::ToolResultEntry, TaskError> {
        use crate::llm::ToolResultEntry;

        // 1. Find tool — if no matching tool, return unknown-tool result immediately (skip HITL)
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == call.tool_name)
            .cloned();

        if tool.is_none() {
            return Ok(ToolResultEntry {
                call_id: call.call_id.clone(),
                content: format!("unknown tool: {}", call.tool_name),
                is_error: true,
                status: crate::tools::ToolStatus::Denied {
                    detail: format!("unknown tool: {}", call.tool_name),
                    // Not `Tool`: an unknown name was never in the request
                    // list, so there is nothing to withdraw. This used to be
                    // special-cased by sniffing the content string for
                    // "unknown tool" — the exact anti-pattern `ToolStatus`
                    // exists to end.
                    scope: crate::tools::DenialScope::Action,
                },
                images: Vec::new(),
            });
        }

        // Resolve step notifier once (route by task id, fall back to baked notifier).
        // Used for step/started + step/completed in both Allow and Ask arms.
        let step_notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>> = {
            let routed = self
                .client_notifiers
                .lock()
                .await
                .get(task_id)
                .map(|(tx, _)| tx.clone());
            routed.or_else(|| self.notifier.clone())
        };
        let step_id = step_id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

        // 1b. Policy gate: check before executing.
        {
            use mur_common::agent::ToolPolicy;
            let policy = effective_tool_policy(&self.tools_policy, &call.tool_name);
            match policy {
                ToolPolicy::Deny => {
                    return Ok(ToolResultEntry {
                        call_id: call.call_id.clone(),
                        content: format!(
                            "Tool `{}` is denied by policy — it is unavailable for the rest of this turn; do not call it again",
                            call.tool_name
                        ),
                        is_error: true,
                        status: crate::tools::ToolStatus::Denied {
                            detail: format!("Tool `{}` is denied by policy.", call.tool_name),
                            scope: crate::tools::DenialScope::Tool,
                        },
                        images: Vec::new(),
                    });
                }
                ToolPolicy::Allow => {
                    // Execute without HITL gate below.
                    let tool = tool.unwrap();
                    if let Some(ref n) = step_notifier {
                        let _ = n
                            .send(step_notification(
                                "step/started",
                                serde_json::json!({
                                    "step_id": step_id,
                                    "task_id": task_id,
                                    "kind": "tool",
                                    "name": call.tool_name,
                                    "args": call.input,
                                }),
                            ))
                            .await;
                    }
                    let t0 = std::time::Instant::now();
                    let (output, status, is_error, images) = match Self::execute_scoped(
                        tool.as_ref(),
                        task_id,
                        call.input.clone(),
                    )
                    .await
                    {
                        Ok(out) => (self.masked(out.text), out.status, false, out.images),
                        // Same refusal handling as the Ask path below —
                        // there are two execute sites, and a fix that
                        // lands on one of them is not a fix.
                        Err(crate::tools::ToolError::NotAuthorized(msg)) => (
                            format!(
                                "{msg} — `{}` is unavailable for the rest of this turn; do not call it again",
                                call.tool_name
                            ),
                            crate::tools::ToolStatus::Denied {
                                detail: msg,
                                scope: crate::tools::DenialScope::Tool,
                            },
                            true,
                            Vec::new(),
                        ),
                        Err(e) => (
                            format!("tool error: {e}"),
                            crate::tools::ToolStatus::Failed { exit_code: -1 },
                            true,
                            Vec::new(),
                        ),
                    };
                    if let Some(ref n) = step_notifier {
                        let (out, truncated, full_len) = cap_step_output(&output);
                        let _ = n
                            .send(step_notification(
                                "step/completed",
                                serde_json::json!({
                                    "step_id": step_id,
                                    "task_id": task_id,
                                    "ok": !is_error,
                                    "output": out,
                                    "truncated": truncated,
                                    "full_len": full_len,
                                    "error": if is_error {
                                        serde_json::Value::String(output.clone())
                                    } else {
                                        serde_json::Value::Null
                                    },
                                    "denied": matches!(status, crate::tools::ToolStatus::Denied { .. }),
                                    "running": matches!(status, crate::tools::ToolStatus::Running { .. }),
                                    "duration_ms": t0.elapsed().as_millis() as u64,
                                }),
                            ))
                            .await;
                    }
                    return Ok(ToolResultEntry {
                        call_id: call.call_id.clone(),
                        content: output,
                        is_error,
                        status,
                        images,
                    });
                }
                ToolPolicy::Ask => {
                    // P3: the decision was made in `gate_response`, before any
                    // call of this response ran. Absent = the gate never saw
                    // this call; deny, never execute.
                    let decision = decision.unwrap_or(crate::hitl::HitlDecision {
                        allow: false,
                        reason: Some("no approval decision for this call".into()),
                        surface: None,
                    });
                    if !decision.allow {
                        return Err(task_error(
                            "hitl_denied",
                            deny_message(decision.reason.as_deref()),
                            false,
                        ));
                    }
                    // Approved: fall through to execute below.
                }
            }
        }

        // 2. Execute the tool
        let tool = tool.unwrap();
        if let Some(ref n) = step_notifier {
            let _ = n
                .send(step_notification(
                    "step/started",
                    serde_json::json!({
                        "step_id": step_id,
                        "task_id": task_id,
                        "kind": "tool",
                        "name": call.tool_name,
                        "args": call.input,
                    }),
                ))
                .await;
        }
        let t0_ask = std::time::Instant::now();
        let (output, status, is_error, images) = match Self::execute_scoped(
            tool.as_ref(),
            task_id,
            call.input.clone(),
        )
        .await
        {
            Ok(out) => (self.masked(out.text), out.status, false, out.images),
            // A refusal is terminal for the tool this turn (spec §3.8): say
            // so once; the loop withdraws it from the next request.
            Err(crate::tools::ToolError::NotAuthorized(msg)) => (
                format!(
                    "{msg} — `{}` is unavailable for the rest of this turn; do not call it again",
                    call.tool_name
                ),
                crate::tools::ToolStatus::Denied {
                    detail: msg,
                    scope: crate::tools::DenialScope::Tool,
                },
                true,
                Vec::new(),
            ),
            Err(e) => (
                format!("tool error: {e}"),
                crate::tools::ToolStatus::Failed { exit_code: -1 },
                true,
                Vec::new(),
            ),
        };
        if let Some(ref n) = step_notifier {
            let (out, truncated, full_len) = cap_step_output(&output);
            let _ = n
                .send(step_notification(
                    "step/completed",
                    serde_json::json!({
                        "step_id": step_id,
                        "task_id": task_id,
                        "ok": !is_error,
                        "output": out,
                        "truncated": truncated,
                        "full_len": full_len,
                        "error": if is_error {
                            serde_json::Value::String(output.clone())
                        } else {
                            serde_json::Value::Null
                        },
                        "denied": matches!(status, crate::tools::ToolStatus::Denied { .. }),
                        "running": matches!(status, crate::tools::ToolStatus::Running { .. }),
                        "duration_ms": t0_ask.elapsed().as_millis() as u64,
                    }),
                ))
                .await;
        }

        // The HITL approval gate now runs PRE-execution in the `Ask` policy
        // arm above (issue #3), so by the time we reach here the tool has been
        // approved and executed. Return its output.
        Ok(ToolResultEntry {
            call_id: call.call_id.clone(),
            content: output,
            is_error,
            status,
            images,
        })
    }
}
