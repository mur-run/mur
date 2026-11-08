//! `--no-hitl`: a one-boot waiver for asks that came only from a default or a
//! clamp at a waivable tier. Not a wider grant ceiling — see
//! [`super::tier_may_be_waived`].
//!
//! The waiver is applied after the decision store is consulted, so a
//! remembered human `Deny` still wins, and what it settles is never written
//! back: the next boot asks again.

use super::{RiskTier, tier_may_be_waived};
use crate::agent::OverrideRecord;

/// Where an ask came from. A flag set, not a variant: one call can ask for
/// more than one reason, and a raise must veto even when a default or a clamp
/// would have asked anyway. `Default` (all false) is never waived, so a call
/// whose origin was never computed fails closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AskOrigin {
    /// The deciding rule is the user's own explicit `ask`.
    pub explicit: bool,
    /// No rule decided; the policy default asked.
    pub default: bool,
    /// An `allow` lifted to `ask` by a declared risk, a builtin floor, or a
    /// per-call (bash) floor.
    pub clamp: bool,
    /// The model raised this call's tier itself (intent lift).
    pub intent_raised: bool,
    /// A `pre_tool_use` safety hook asked (B0 rule 4: a side effect after
    /// untrusted input). A question about the turn, not the tool — never
    /// waived, even when a default or a clamp would have asked anyway.
    pub hook_raised: bool,
}

/// The audit-reason prefix on every decision a waiver settles, so it can never
/// be mistaken for a human answer.
pub const STARTUP_WAIVER_REASON_PREFIX: &str = "startup-override:";

/// One boot's `--no-hitl`.
///
/// Deliberately has no `Deserialize`, no `Default` and no public fields: the
/// only way to hold one is [`StartupWaiver::from_record`] on an override record
/// the runtime has already validated and claimed.
#[derive(Clone, Debug)]
pub struct StartupWaiver {
    nonce: String,
}

impl StartupWaiver {
    /// `Some` iff the accepted override asked for `no_hitl`.
    pub fn from_record(record: &OverrideRecord) -> Option<Self> {
        record.flags.no_hitl.then(|| Self {
            nonce: record.nonce.clone(),
        })
    }

    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// `startup-override:<nonce>` — the reason recorded on a waived decision.
    pub fn reason(&self) -> String {
        format!("{STARTUP_WAIVER_REASON_PREFIX}{}", self.nonce)
    }

    /// May this boot settle the ask without a human? Checked in this order:
    /// an intent raise or a hook ask vetoes; an explicit `ask` is kept; only a
    /// default or a clamp is eligible; and then only at a waivable tier.
    pub fn may_waive(&self, origin: AskOrigin, tier: Option<RiskTier>) -> bool {
        origin_eligible(origin) && tier_may_be_waived(tier)
    }

    /// Would this ask have been waived but for its tier? True when the
    /// origin alone is eligible and the tier (or a spend tool's name) is
    /// what keeps a human in the loop. Used only to explain the eventual
    /// timeout (#2313); it never changes what is waived.
    pub fn held_by_tier(&self, origin: AskOrigin, tier: Option<RiskTier>, spend: bool) -> bool {
        origin_eligible(origin) && (spend || !tier_may_be_waived(tier))
    }
}

/// The origin half of the waiver rule, shared by [`StartupWaiver::may_waive`]
/// and [`StartupWaiver::held_by_tier`] so the explanation cannot drift from
/// what is actually waived: an intent raise or a hook ask vetoes, an explicit
/// `ask` is kept, and only a default or a clamp is eligible.
fn origin_eligible(origin: AskOrigin) -> bool {
    !(origin.intent_raised || origin.hook_raised || origin.explicit)
        && (origin.default || origin.clamp)
}

/// `refusal_code` on a `--no-hitl` ask that timed out because its tier is
/// never waived (#2313).
pub const TIER_NOT_WAIVED: &str = "tier_not_waived";

/// What the agent is told to do next. No "use another form" hint: that
/// reads as "reword the call so it classifies lower", the evasion #2314 closed.
const TIER_NOT_WAIVED_NEXT: &str =
    "Retrying will wait again. Ask the user to approve it, or continue without this operation";

fn tier_not_waived_clause(tier: Option<RiskTier>) -> String {
    let tier = match tier {
        Some(t) => format!("{t:?}"),
        None => "undeclared".to_string(),
    };
    format!(
        "this call is {tier}-tier, and --no-hitl never waives \
         Spend, Privileged or undeclared-tier calls"
    )
}

/// What the agent reads when such an ask times out. Starts with the gate's
/// own timeout text so a reader of the reason still sees it was a timeout.
pub fn tier_not_waived_reason(timed_out: &str, tier: Option<RiskTier>) -> String {
    format!(
        "{timed_out}: {}, so it waited for a human and nobody answered. {TIER_NOT_WAIVED_NEXT}",
        tier_not_waived_clause(tier)
    )
}

/// The same explanation without the timeout framing, for a refusal whose
/// own text already says the wait ran out (the headless parked ask).
pub fn tier_not_waived_note(tier: Option<RiskTier>) -> String {
    format!("{}. {TIER_NOT_WAIVED_NEXT}", tier_not_waived_clause(tier))
}

#[cfg(test)]
#[path = "startup_waiver_tests.rs"]
mod tests;
