//! Gate B, batched: every `Ask` call of one LLM response is resolved before
//! any of them runs — a settled decision from the store, or ONE
//! `tool/approval_needed` carrying all of them. Each call keeps its own
//! `hitl_id` and its own oneshot, so `tool/hitl_respond` is unchanged and no
//! single answer can release more than one call.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::store::{DecisionStore, Settled, chat_action_hash};
use super::{HitlApprovals, HitlDecision};

/// One `Ask` call awaiting a decision.
pub struct PendingCall {
    pub call_id: String,
    pub step_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub action_hash: String,
}

pub struct BatchGate<'a> {
    pub task_id: &'a str,
    pub timeout: Duration,
    pub approvals: &'a HitlApprovals,
    pub notifier: &'a tokio::sync::mpsc::Sender<serde_json::Value>,
    pub store: Option<&'a Arc<dyn DecisionStore>>,
}

const TIMED_OUT: &str = "timed out";

impl BatchGate<'_> {
    /// Resolve every pending call. Returns `call_id → decision`; every input
    /// call has an entry (timeout and store-denials included).
    pub async fn resolve(&self, calls: Vec<PendingCall>) -> HashMap<String, HitlDecision> {
        let mut out = HashMap::with_capacity(calls.len());
        let mut ask = Vec::new();
        for c in calls {
            let settled = match self.store {
                Some(s) => s.lookup(&c.action_hash).await,
                None => None,
            };
            match settled {
                Some(Settled::Allow) => {
                    out.insert(c.call_id, remembered(true));
                }
                Some(Settled::Deny) => {
                    out.insert(c.call_id, remembered(false));
                }
                None => ask.push(c),
            }
        }
        if ask.is_empty() {
            return out;
        }
        let batch_id = uuid::Uuid::now_v7().to_string();
        let mut waiting = Vec::with_capacity(ask.len());
        let mut wire = Vec::with_capacity(ask.len());
        {
            let mut pa = self.approvals.lock().await;
            for c in ask {
                let hitl_id = uuid::Uuid::now_v7().to_string();
                let (tx, rx) = tokio::sync::oneshot::channel::<HitlDecision>();
                pa.insert(hitl_id.clone(), tx);
                wire.push(serde_json::json!({
                    "hitl_id": hitl_id,
                    "step_id": c.step_id,
                    "tool_name": c.tool_name,
                    "tool_input": c.tool_input,
                    "action_hash": c.action_hash,
                }));
                waiting.push((c, hitl_id, rx));
            }
        }
        let first = &wire[0];
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool/approval_needed",
            "params": {
                "batch_id": batch_id,
                "task_id": self.task_id,
                "timeout_ms": self.timeout.as_millis() as u64,
                // Legacy single-call fields = the first call, for clients that
                // predate `calls`. Their answer lands on calls[0]; the rest
                // time out and deny, which is the fail-closed direction.
                "step_id": first["step_id"],
                "hitl_id": first["hitl_id"],
                "tool_name": first["tool_name"],
                "tool_input": first["tool_input"],
                "calls": wire,
            }
        });
        let _ = self.notifier.send(notification).await;

        // One deadline for the whole batch; each oneshot is awaited in turn
        // against what is left of it.
        let deadline = tokio::time::Instant::now() + self.timeout;
        for (c, hitl_id, rx) in waiting {
            let decision = match tokio::time::timeout_at(deadline, rx).await {
                Ok(Ok(d)) => d,
                _ => {
                    self.approvals.lock().await.remove(&hitl_id);
                    HitlDecision {
                        allow: false,
                        reason: Some(TIMED_OUT.into()),
                        surface: None,
                    }
                }
            };
            // A timeout is not a human decision: recording it would deny the
            // next identical call without asking anyone. Only answers settle.
            let answered = decision.reason.as_deref() != Some(TIMED_OUT);
            if let (Some(s), true) = (self.store, answered) {
                s.record(mur_common::hitl::HitlResponse {
                    hitl_id: hitl_id.clone(),
                    action_hash: c.action_hash.clone(),
                    allow: decision.allow,
                    reason: decision.reason.clone().unwrap_or_default(),
                    surface: decision.surface.clone().unwrap_or_else(|| "unknown".into()),
                })
                .await;
            }
            out.insert(c.call_id, decision);
        }
        out
    }
}

fn remembered(allow: bool) -> HitlDecision {
    HitlDecision {
        allow,
        reason: Some(if allow {
            "approved earlier".into()
        } else {
            "denied earlier".into()
        }),
        surface: None,
    }
}

/// Build the pending entry for one call. Separate so `task_runner` never
/// spells the hash itself.
pub fn pending(agent: &str, step_id: String, call: &crate::llm::ToolCallResult) -> PendingCall {
    PendingCall {
        call_id: call.call_id.clone(),
        step_id,
        action_hash: chat_action_hash(&call.tool_name, &call.input, agent),
        tool_name: call.tool_name.clone(),
        tool_input: call.input.clone(),
    }
}
