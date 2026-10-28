//! #2085: settle the `Ask` calls of a caller that cannot approve
//! (`mur agent send`, a `channel/delegate` turn).
//!
//! The decision store answers first: an allow or deny the human already gave
//! — from `mur agent cli --ask`, the Hub, any surface that writes the signed
//! decision log — applies here exactly as it would interactively, so the
//! headless path honours a one-off approval without a permanent `tool-allow`.
//!
//! A call nobody has decided then goes one of two ways:
//! - the turn opened an approval window (`mur agent send --wait-approval`):
//!   it is parked on the ordinary gate — same `hitl_id`, same
//!   `tool/hitl_respond` and approval authority — and listed by
//!   `hitl/pending`, until a human answers or the window (clamped to the
//!   turn's deadline) runs out;
//! - otherwise, or once the window has run out: refused, marked
//!   `awaiting_human` so the caller sees `hitl_approval_required` instead of
//!   `hitl_denied`.
//!
//! Nothing unapproved runs either way, and a wait that runs out is never
//! recorded as a decision.

use std::collections::HashMap;

use super::GuardedToolCall;
use crate::hitl::HitlDecision;
use crate::hitl::batch::{BatchGate, PendingCall, is_timeout};
use crate::hitl::store::Settled;
use crate::llm::ToolCallResult;
use crate::task_runner::deny_hint::ask_cause;
use crate::task_runner::{decide_without_asking, denied_args_preview};

/// Appended to the refusal reason when a parked call's wait ran out, so the
/// caller can tell "nobody was asked" from "nobody answered in time".
const WAIT_EXPIRED: &str = "nobody answered while the request waited";

impl GuardedToolCall {
    pub(super) async fn settle_headless(
        &self,
        task_id: &str,
        calls: &[ToolCallResult],
        pending: Vec<PendingCall>,
        sink: &tokio::sync::mpsc::Sender<serde_json::Value>,
        out: &mut HashMap<String, HitlDecision>,
    ) {
        // Same rule as the asking gate: a one-shot turn neither reads nor
        // writes standing decisions (#1783).
        let remember = !self.one_shot_approvals.lock().await.contains(task_id);
        let mut undecided = Vec::new();
        for c in pending {
            let settled = match &self.decision_store {
                Some(s) if remember && !c.one_shot => s.lookup(&c.action_hash).await,
                _ => None,
            };
            match settled {
                Some(s) => {
                    out.insert(c.call_id, remembered(s));
                }
                None => undecided.push(c),
            }
        }
        let undecided = self.apply_waiver(task_id, undecided, remember, out).await;
        if undecided.is_empty() {
            return;
        }
        for c in &undecided {
            self.record_pending_ceiling(task_id, calls, c).await;
        }
        let parked = match (self.parking.remaining(task_id), &self.pending_approvals) {
            (Some(wait), Some(pa)) => {
                let gate = BatchGate {
                    task_id,
                    timeout: wait,
                    approvals: pa,
                    notifier: sink,
                    store: self.decision_store.as_ref(),
                    shim_trust: self.shim_trust.as_ref(),
                    remember,
                    parking: Some(&self.parking),
                };
                tracing::info!(
                    task_id,
                    calls = undecided.len(),
                    wait_secs = wait.as_secs(),
                    "headless caller is waiting: approval parked for a human"
                );
                let answers = gate.ask(undecided.clone()).await;
                Some(answers)
            }
            _ => None,
        };
        for c in undecided {
            match parked.as_ref().and_then(|a| a.get(&c.call_id)) {
                Some(d) if !is_timeout(d) => {
                    out.insert(c.call_id, d.clone());
                }
                answer => {
                    let expired = answer.is_some();
                    out.insert(c.call_id.clone(), self.refuse(task_id, calls, &c, expired));
                }
            }
        }
    }

    /// The fail-closed answer for a call no human has decided.
    fn refuse(
        &self,
        task_id: &str,
        calls: &[ToolCallResult],
        c: &PendingCall,
        expired: bool,
    ) -> HitlDecision {
        let input = calls
            .iter()
            .find(|k| k.call_id == c.call_id)
            .map(|k| &k.input)
            .unwrap_or(&c.tool_input);
        let cause = ask_cause(&self.tools_policy, &c.tool_name, Some(input));
        let mut d = decide_without_asking(Some(false), &self.agent_name, &c.tool_name, &cause)
            .expect("Some(false) always refuses");
        if expired {
            d.reason = Some(match d.reason.take() {
                Some(r) => format!("{WAIT_EXPIRED}; {r}"),
                None => WAIT_EXPIRED.to_string(),
            });
        }
        tracing::warn!(
            task_id,
            tool = %c.tool_name,
            args = %denied_args_preview(input),
            expired,
            "tool call refused: caller cannot approve and no decision is remembered"
        );
        d
    }
}

/// Same wording as the asking gate's remembered answers.
fn remembered(s: Settled) -> HitlDecision {
    let allow = matches!(s, Settled::Allow);
    HitlDecision {
        allow,
        reason: Some(
            if allow {
                "approved earlier"
            } else {
                "denied earlier"
            }
            .into(),
        ),
        surface: None,
        awaiting_human: false,
        refusal_code: None,
    }
}
