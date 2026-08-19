//! Risk-tiered HITL vocabulary shared across the executor, runtime, and surfaces.

use serde::{Deserialize, Serialize};

/// How risky an action is. `Ord` is severity order: `Read` < … < `Privileged`.
/// Tier is resolved most-restrictive-wins and is NEVER LLM-asserted.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RiskTier {
    Read,
    Write,
    NetworkEgress,
    Spend,
    Destructive,
    Privileged,
}

/// What the gate does for a tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HitlMode {
    /// Run unattended (read tier): a post-hoc audit event is fine.
    Auto,
    /// Pre-execution human approval required.
    Ask,
    /// Refuse pre-emptively.
    Deny,
}

/// Default gate mode for a tier. Read runs unattended; everything mutating asks.
/// A channel policy floor (future) may tighten Ask→Deny but never loosen.
pub fn default_mode(tier: RiskTier) -> HitlMode {
    match tier {
        RiskTier::Read => HitlMode::Auto,
        _ => HitlMode::Ask,
    }
}

/// What an Ask-tier gate does when nobody has answered yet.
///
/// This is a policy floor, chosen by the run's owner — it may only tighten the
/// outcome, never approve anything. `Deny` short-circuits before any lookup so
/// a fleet declared free of risk-tiered work stays that way even if some older
/// approval for the same action is still on the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unanswered {
    /// Park the request durably and report the step blocked. Nobody waits; an
    /// approval arriving later releases the gate on a subsequent run. The
    /// default when no human is watching.
    Defer,
    /// Block the caller, polling until the gate timeout. The default when a
    /// terminal is attached, and the right choice for an unattended run that
    /// somebody IS watching on another surface.
    Wait,
    /// Refuse every Ask-tier action outright, without writing a request. For a
    /// run that must never reach for a human — the failure is immediate and
    /// legible instead of a request nobody will answer.
    Deny,
}

impl Default for Unanswered {
    /// The strict end of the three: a policy built without stating a mode must
    /// never be the one that waits or lets something through.
    fn default() -> Self {
        Unanswered::Defer
    }
}

/// May a run's owner take standing responsibility for this tier in config —
/// i.e. pre-approve it once instead of being asked every time?
///
/// Capped at `Write` deliberately. A standing grant is real authority handed
/// to an unattended process, so widening it is a decision to make in code with
/// its reasoning written down, never something a user acquires by typing one
/// more word into a YAML file. `Spend`, `Destructive` and `Privileged` are
/// exactly the actions whose cost a human cannot undo by noticing later, and
/// `NetworkEgress` is how data leaves — none of them belongs behind a config
/// line today.
pub fn tier_may_be_granted(tier: RiskTier) -> bool {
    matches!(tier, RiskTier::Read | RiskTier::Write)
}

/// `EventKind::HitlRequest` payload: the durable, pinned approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlRequest {
    pub hitl_id: String,
    /// SHA-256 of the canonical action (see `mur-core` `hitl::pin`).
    pub action_hash: String,
    pub tier: RiskTier,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub step_or_call_id: String,
    pub agent_id: String,
    pub timeout_ms: u64,
    pub summary: String,
}

/// `EventKind::HitlResponse` payload: the human's decision, echoing the pin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlResponse {
    pub hitl_id: String,
    pub action_hash: String,
    pub allow: bool,
    #[serde(default)]
    pub reason: String,
    /// "cli" | "hub" | "ios" | "auto".
    pub surface: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_orders_by_severity_and_maps_mode() {
        assert!(RiskTier::Read < RiskTier::Destructive);
        assert!(RiskTier::Write < RiskTier::Privileged);
        assert_eq!(default_mode(RiskTier::Read), HitlMode::Auto);
        assert_eq!(default_mode(RiskTier::Destructive), HitlMode::Ask);
    }

    #[test]
    fn hitl_payloads_round_trip() {
        let req = HitlRequest {
            hitl_id: "h1".into(),
            action_hash: "abc".into(),
            tier: RiskTier::Destructive,
            tool_name: "bash".into(),
            tool_input: serde_json::json!({ "cmd": "rm -rf x" }),
            step_or_call_id: "s0".into(),
            agent_id: "mur".into(),
            timeout_ms: 300_000,
            summary: "delete x".into(),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: HitlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.tier, RiskTier::Destructive);
        assert_eq!(back.action_hash, "abc");
    }

    /// The grantable ceiling. Widening this list is a security decision that
    /// belongs in a commit message, not a YAML typo — the test exists so the
    /// reviewer has to read the reasoning right here.
    #[test]
    fn tier_grant_ceiling_is_write() {
        assert!(tier_may_be_granted(RiskTier::Read));
        assert!(tier_may_be_granted(RiskTier::Write));
        assert!(!tier_may_be_granted(RiskTier::NetworkEgress));
        assert!(!tier_may_be_granted(RiskTier::Spend));
        assert!(!tier_may_be_granted(RiskTier::Destructive));
        assert!(!tier_may_be_granted(RiskTier::Privileged));
    }
}
