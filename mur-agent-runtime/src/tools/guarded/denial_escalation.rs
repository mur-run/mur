//! #2581 part 2: escalate after a denial, for the rest of the conversation.
//!
//! A reviewer refused `rm -rf`; the next turn re-ran the same delete as a
//! Python heredoc, which tiered `Write` and ran unasked. Text matching can
//! never see every spelling of a delete inside an interpreter, so once a
//! `Destructive` call has been refused, interpreter / shell calls tier
//! `Destructive` too — in that turn and every later turn of the same
//! conversation ([`DenialLedger`]), until the human approves the refused call.
//!
//! - Adds no prompt in a conversation where nothing was refused.
//! - Any refusal counts — a human's deny, a remembered deny, and the
//!   fail-closed "approval required" a headless caller gets (the incident).
//! - The lift rides the same `raised` map as an intent raise, so the policy
//!   arm, the prompt's tier and `handle_tool_call` treat it exactly alike:
//!   it can turn an `Allow` into an ask, never an ask into an allow.
//!
//! [`DenialLedger`]: crate::task_runner::denial_ledger::DenialLedger

use std::collections::HashMap;

use mur_common::hitl::RiskTier;
use mur_common::hitl::tool_tier::{classify, escalates_after_denial};

use super::GuardedToolCall;
use crate::hitl::HitlDecision;
use crate::hitl::store::{Settled, chat_action_hash};
use crate::llm::ToolCallResult;
use crate::task_runner::approval_risk;

/// The tier at and above which a refusal is recorded, and the tier an
/// interpreter call is lifted to while a record is open.
const ARMING_TIER: RiskTier = RiskTier::Destructive;

impl GuardedToolCall {
    /// Every call lift for this response: the intent check's raises, plus
    /// [`ARMING_TIER`] for interpreter calls while a refusal is open.
    /// Most-restrictive-wins where both apply.
    pub(super) async fn raises(
        &self,
        task_id: &str,
        calls: &[ToolCallResult],
    ) -> HashMap<String, RiskTier> {
        let mut raised = self.intent_raises(task_id, calls).await;
        if !self.refusal_is_open(task_id).await {
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

    /// Record every call `out` refuses whose gated tier is at least
    /// [`ARMING_TIER`], under this turn, by the hash the decision store keys
    /// its answers on — so an approval of exactly that call clears it.
    pub(super) fn arm_denial_escalation(
        &self,
        task_id: &str,
        calls: &[ToolCallResult],
        raised: &HashMap<String, RiskTier>,
        out: &HashMap<String, HitlDecision>,
    ) {
        for c in calls {
            let refused = out.get(&c.call_id).is_some_and(|d| !d.allow);
            if !refused || self.gated_tier(c, raised).is_none_or(|t| t < ARMING_TIER) {
                continue;
            }
            tracing::info!(
                agent = %self.agent_name,
                task_id,
                tool = %c.tool_name,
                "destructive call refused; interpreter calls escalate until it is approved"
            );
            self.denial_ledger.record(
                task_id,
                chat_action_hash(&c.tool_name, &c.input, &self.agent_name),
            );
        }
    }

    /// Is a refusal still open for this turn? Records the human has since
    /// approved (an allow for that hash in the decision store) are closed
    /// here first, so the rest of the conversation inherits only what is left.
    async fn refusal_is_open(&self, task_id: &str) -> bool {
        let open = self.denial_ledger.open(task_id);
        if open.is_empty() {
            return false;
        }
        let mut approved = Vec::new();
        if let Some(store) = &self.decision_store {
            for h in &open {
                if store.lookup(h).await == Some(Settled::Allow) {
                    approved.push(h.clone());
                }
            }
        }
        if !approved.is_empty() {
            self.denial_ledger.clear(task_id, &approved);
        }
        approved.len() < open.len()
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
