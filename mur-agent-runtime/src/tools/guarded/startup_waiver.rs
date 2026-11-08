//! `--no-hitl` in the gate: settle the asks this boot's [`StartupWaiver`]
//! covers, after the decision store has had its say. Shared by the asking
//! gate and the headless path so the two cannot drift.
//!
//! Behavior tests live at the runner seam: `task_runner/tests/startup_waiver.rs`.
//!
//! What it settles is never written to the decision store (the store is only
//! written from `BatchGate::ask`'s human answers), so the next boot asks again.

use std::collections::HashMap;

use mur_common::hitl::RiskTier;
use mur_common::hitl::startup_waiver::{StartupWaiver, TIER_NOT_WAIVED, tier_not_waived_reason};
use mur_common::hitl::tool_tier::{is_spend_tool, spend_floor};
use mur_common::telemetry::METHOD_STARTUP_WAIVED;

use super::GuardedToolCall;
use crate::hitl::HitlDecision;
use crate::hitl::batch::{PendingCall, TIMED_OUT, is_timeout};
use crate::hitl::store::Settled;
use crate::telemetry_writer::Event;

impl GuardedToolCall {
    /// Returns the calls still needing a human; settles the rest into `out`.
    ///
    /// `remember` is the gate's own flag. With it set, the store was already
    /// consulted for both answers, so whatever is left has no entry. Without
    /// it (a one-shot task), a remembered `Deny` is still honored — a waiver
    /// must never run what a human refused — while a remembered `Allow` stays
    /// ignored, which is what one-shot means.
    pub(super) async fn apply_waiver(
        &self,
        task_id: &str,
        ask: Vec<PendingCall>,
        remember: bool,
        out: &mut HashMap<String, HitlDecision>,
    ) -> Vec<PendingCall> {
        let Some(waiver) = &self.startup_waiver else {
            return ask;
        };
        let mut still = Vec::with_capacity(ask.len());
        for c in ask {
            if !remember
                && !c.one_shot
                && let Some(store) = &self.decision_store
                && store.lookup(&c.action_hash).await == Some(Settled::Deny)
            {
                out.insert(c.call_id, denied_earlier());
                continue;
            }
            let tier = c.risk.max(spend_floor(&c.tool_name));
            // A spend tool is refused by name, not only through `tier`:
            // `RiskTier` orders `Spend` below `Destructive`, so `fleet_run`
            // declared `risk: destructive` would max to a waivable tier.
            if is_spend_tool(&c.tool_name) || !waiver.may_waive(c.origin, tier) {
                still.push(c);
                continue;
            }
            self.record_waived(task_id, waiver, &c, tier);
            out.insert(c.call_id, waived(waiver));
        }
        still
    }

    /// `call_id → tier` for the asks this boot's waiver would have settled
    /// but for their tier (#2313). Empty without a waiver. Read-only: the
    /// calls still go to a human exactly as before.
    pub(super) fn held_by_tier(&self, ask: &[PendingCall]) -> HashMap<String, Option<RiskTier>> {
        let Some(waiver) = &self.startup_waiver else {
            return HashMap::new();
        };
        ask.iter()
            .filter_map(|c| {
                let tier = c.risk.max(spend_floor(&c.tool_name));
                let spend = is_spend_tool(&c.tool_name);
                waiver
                    .held_by_tier(c.origin, tier, spend)
                    .then(|| (c.call_id.clone(), tier))
            })
            .collect()
    }

    fn record_waived(
        &self,
        task_id: &str,
        waiver: &StartupWaiver,
        c: &PendingCall,
        tier: Option<mur_common::hitl::RiskTier>,
    ) {
        tracing::info!(
            agent = %self.agent_name,
            task_id,
            tool = %c.tool_name,
            tier = ?tier,
            nonce = waiver.nonce(),
            "--no-hitl settled an ask without a human"
        );
        if let Some(tx) = &self.telemetry {
            let _ = tx.try_send(Event::HookFired {
                method: METHOD_STARTUP_WAIVED.to_string(),
                attrs: serde_json::json!({
                    mur_common::telemetry::MUR_EVENT_TYPE: METHOD_STARTUP_WAIVED,
                    mur_common::telemetry::MUR_AGENT_NAME: self.agent_name,
                    mur_common::telemetry::MUR_TASK_ID: task_id,
                    "call_id": c.call_id,
                    "tool": c.tool_name,
                    "tier": tier,
                    "nonce": waiver.nonce(),
                }),
            });
        }
    }
}

/// Rewrite the bare "timed out" of an ask held only by its tier into one
/// that names the tier and the waiver. Runs after the gate has decided —
/// and after the store chose not to record the timeout — so neither the
/// outcome nor what is remembered changes, only what the agent is told.
pub(super) fn explain_held(
    out: &mut HashMap<String, HitlDecision>,
    held: &HashMap<String, Option<RiskTier>>,
) {
    for (id, tier) in held {
        if let Some(d) = out.get_mut(id)
            && is_timeout(d)
        {
            d.reason = Some(tier_not_waived_reason(TIMED_OUT, *tier));
            d.refusal_code = Some(TIER_NOT_WAIVED);
        }
    }
}

/// The headless refusal for a parked ask whose wait ran out: same decision,
/// with the tier explanation appended (#2313).
pub(super) fn explain_held_refusal(d: &mut HitlDecision, tier: Option<RiskTier>) {
    let why = tier_not_waived_reason(TIMED_OUT, tier);
    d.reason = Some(match d.reason.take() {
        Some(r) => format!("{r}; {why}"),
        None => why,
    });
}

fn waived(waiver: &StartupWaiver) -> HitlDecision {
    HitlDecision {
        allow: true,
        reason: Some(waiver.reason()),
        surface: None,
        awaiting_human: false,
        refusal_code: None,
    }
}

/// Same shape as the gate's own remembered denial.
fn denied_earlier() -> HitlDecision {
    HitlDecision {
        allow: false,
        reason: Some("denied earlier".into()),
        surface: None,
        awaiting_human: false,
        refusal_code: None,
    }
}
