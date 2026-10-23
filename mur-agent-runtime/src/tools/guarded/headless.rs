//! #2085: settle the `Ask` calls of a caller that cannot approve
//! (`mur agent send`, a `channel/delegate` turn).
//!
//! The decision store answers first: an allow or deny the human already gave
//! — from `mur agent cli --ask`, the Hub, any surface that writes the signed
//! decision log — applies here exactly as it would interactively, so the
//! headless path honours a one-off approval without a permanent `tool-allow`.
//! Only a call nobody has decided is refused, marked `awaiting_human` so the
//! caller sees `hitl_approval_required` instead of `hitl_denied`. Nothing is
//! asked (nobody could answer) and nothing unapproved runs.

use std::collections::HashMap;

use super::GuardedToolCall;
use crate::hitl::HitlDecision;
use crate::hitl::batch::PendingCall;
use crate::hitl::store::Settled;
use crate::llm::ToolCallResult;
use crate::task_runner::deny_hint::ask_cause;
use crate::task_runner::{decide_without_asking, denied_args_preview};

impl GuardedToolCall {
    pub(super) async fn settle_headless(
        &self,
        task_id: &str,
        calls: &[ToolCallResult],
        pending: Vec<PendingCall>,
        out: &mut HashMap<String, HitlDecision>,
    ) {
        // Same rule as the asking gate: a one-shot turn neither reads nor
        // writes standing decisions (#1783).
        let remember = !self.one_shot_approvals.lock().await.contains(task_id);
        for c in pending {
            let settled = match &self.decision_store {
                Some(s) if remember => s.lookup(&c.action_hash).await,
                _ => None,
            };
            if let Some(s) = settled {
                out.insert(c.call_id, remembered(s));
                continue;
            }
            self.record_pending_ceiling(task_id, calls, &c).await;
            let input = calls
                .iter()
                .find(|k| k.call_id == c.call_id)
                .map(|k| &k.input)
                .unwrap_or(&c.tool_input);
            let cause = ask_cause(&self.tools_policy, &c.tool_name, Some(input));
            let d = decide_without_asking(Some(false), &self.agent_name, &c.tool_name, &cause)
                .expect("Some(false) always refuses");
            tracing::warn!(
                task_id,
                tool = %c.tool_name,
                args = %denied_args_preview(input),
                "tool call refused: caller cannot approve and no decision is remembered"
            );
            out.insert(c.call_id, d);
        }
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
    }
}
