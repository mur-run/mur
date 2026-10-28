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
    /// an intent raise vetoes; an explicit `ask` is kept; only a default or a
    /// clamp is eligible; and then only at a waivable tier.
    pub fn may_waive(&self, origin: AskOrigin, tier: Option<RiskTier>) -> bool {
        if origin.intent_raised || origin.explicit {
            return false;
        }
        if !(origin.default || origin.clamp) {
            return false;
        }
        tier_may_be_waived(tier)
    }
}

#[cfg(test)]
#[path = "startup_waiver_tests.rs"]
mod tests;
