//! #2581 part 2: escalate after a denial, scoped to the turn.
//!
//! A reviewer refused `rm -rf` re-ran the same delete as a Python heredoc,
//! which tiered `Write` and ran unasked. Text matching can never see every
//! spelling of a delete inside an interpreter, so once a human (or the
//! fail-closed default) has said no to a `Destructive` call, every later
//! interpreter / shell call of THAT turn is lifted to `Destructive` too.
//!
//! - Adds no prompt in a turn where nothing was refused.
//! - Any refusal counts — a human's deny, a remembered deny, and the
//!   fail-closed "approval required" a headless caller gets (the incident).
//! - The lift rides the same `raised` map as an intent raise, so the policy
//!   arm, the prompt's tier and `handle_tool_call` treat it exactly alike:
//!   it can turn an `Allow` into an ask, never an ask into an allow.
//! - Cleared by `TaskRunner::unregister_client_notifier` with the turn's
//!   other per-task state; a new turn starts clean.

use std::collections::HashMap;

use mur_common::hitl::RiskTier;
use mur_common::hitl::tool_tier::{classify, escalates_after_denial};

use super::GuardedToolCall;
use crate::hitl::HitlDecision;
use crate::llm::ToolCallResult;
use crate::task_runner::approval_risk;

/// The tier at and above which a refusal arms the turn, and the tier an
/// interpreter call of an armed turn is lifted to.
const ARMING_TIER: RiskTier = RiskTier::Destructive;

impl GuardedToolCall {
    /// Every call lift for this response: the intent check's raises, plus
    /// [`ARMING_TIER`] for interpreter calls in an already-armed turn.
    /// Most-restrictive-wins where both apply.
    pub(super) async fn raises(
        &self,
        task_id: &str,
        calls: &[ToolCallResult],
    ) -> HashMap<String, RiskTier> {
        let mut raised = self.intent_raises(task_id, calls).await;
        if !self.turn_is_armed(task_id) {
            return raised;
        }
        for c in calls {
            if escalates_after_denial(&c.tool_name, Some(&c.input))
                && classify(&c.tool_name, Some(&c.input)) < ARMING_TIER
            {
                let lift = raised.entry(c.call_id.clone()).or_insert(ARMING_TIER);
                *lift = (*lift).max(ARMING_TIER);
            }
        }
        raised
    }

    /// Arm the turn if `out` refuses any call whose gated tier is at least
    /// [`ARMING_TIER`]. Arming is sticky for the rest of the turn.
    pub(super) fn arm_denial_escalation(
        &self,
        task_id: &str,
        calls: &[ToolCallResult],
        raised: &HashMap<String, RiskTier>,
        out: &HashMap<String, HitlDecision>,
    ) {
        let refused_destructive = calls.iter().any(|c| {
            out.get(&c.call_id).is_some_and(|d| !d.allow)
                && self.gated_tier(c, raised).is_some_and(|t| t >= ARMING_TIER)
        });
        if !refused_destructive {
            return;
        }
        tracing::info!(
            agent = %self.agent_name,
            task_id,
            "destructive call refused; interpreter calls escalate for the rest of this turn"
        );
        self.denied_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task_id.to_string());
    }

    fn turn_is_armed(&self, task_id: &str) -> bool {
        self.denied_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(task_id)
    }

    /// The tier the gate asked about for `c` — the same `approval_risk ∨
    /// lift` the pending prompt carries.
    fn gated_tier(
        &self,
        c: &ToolCallResult,
        raised: &HashMap<String, RiskTier>,
    ) -> Option<RiskTier> {
        approval_risk(&self.tools_policy, &c.tool_name, Some(&c.input))
            .max(raised.get(&c.call_id).copied())
    }
}
