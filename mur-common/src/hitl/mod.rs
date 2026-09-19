//! Risk-tiered HITL vocabulary shared across the executor, runtime, and surfaces.

use serde::{Deserialize, Serialize};

pub mod pin;

/// Approvals and denials settle a gate for this long. Content staleness is
/// already handled by the hash pin (any input change = a different hash); the
/// TTL bounds TIME staleness, so a weeks-old approval cannot release a gate
/// nobody remembers granting. Shared by gate A (`mur-core::hitl::gate`) and
/// gate B (`mur-agent-runtime::hitl::store`) — one number, or the two gates
/// remember for different lengths and the Hub cannot explain why.
pub const APPROVAL_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Pure TTL predicate — split out so the boundary is testable without
/// backdating channel events.
pub fn within_approval_ttl(
    event_ts: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    (now - event_ts).num_seconds() <= APPROVAL_TTL_SECS
}

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

/// How far the agent carries a turn on its own before handing back.
///
/// ORTHOGONAL to `HitlMode`/`RiskTier`. Those answer "may this ACTION run?"
/// and are enforced per tool call; this answers "is the TURN over?" and is
/// enforced once, at the loop's termination branch. Neither may overrule the
/// other: `Continue` never releases a risk gate, and an approved gate never
/// extends a turn. Issue #001 is what happens when only the prompt layer
/// carries this — the model reads "已授權工作持續推進" as a suggestion because
/// nothing in the runtime ever re-entered the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Autonomy {
    /// Mode 1, 持續推進 — a turn that ends with work still open is nudged back
    /// into the loop instead of returning. Never implicit, not even for
    /// unattended runs: handing an agent the right to keep going is written
    /// down in a profile, because nobody is in the room to take it back.
    Continue,
    /// Mode 2, 需要再審核 — the agent finishes its own work but must present it
    /// for review before anything further; the turn ends where it would anyway.
    Review,
    /// Mode 3, 用戶審核 — hand back at every natural stop. The strictest of the
    /// three and the default when nothing is stated.
    Ask,
}

impl Default for Autonomy {
    /// The strict end, matching `Unanswered::default()`: a policy assembled
    /// without stating a mode must never be the one that keeps going by
    /// itself.
    fn default() -> Self {
        Autonomy::Ask
    }
}

/// How many times one turn may be nudged onward. Bounded, and small: the
/// iteration ceiling and the stuck clock are the real budgets, and a
/// continuation that could fire endlessly would quietly convert both into a
/// suggestion. One nudge is enough to fix #001 (the model stopped once, mid
/// task) without inventing a second, parallel loop.
pub const MAX_CONTINUATIONS: u32 = 1;

/// Why a turn was NOT continued. Every variant is a thing the settlement card
/// can print, because "it just stopped" is the bug being fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinueVeto {
    /// Policy says hand back — `Review` or `Ask`.
    Policy,
    /// A1: the turn did not end cleanly (ceiling, loop, deadline, stuck,
    /// truncation). Those stops already have their own graceful exit and a
    /// nudge would fight it.
    UnCleanStop,
    /// A2: a risk gate blocked, denied or deferred something this turn. The
    /// human IS the next step; nudging would spin against a closed gate.
    GateBlocked,
    /// The nudge budget for this turn is spent.
    BudgetSpent,
}

/// The whole continuation decision, as one pure function so the policy is
/// testable without a model, a gate, or a clock.
///
/// `clean_stop` is "the model ended the turn of its own accord". `gate_blocked`
/// is "at least one action this turn was refused, denied or parked". Both are
/// facts the loop already holds at the termination branch.
pub fn should_continue(
    autonomy: Autonomy,
    clean_stop: bool,
    gate_blocked: bool,
    continuations_used: u32,
) -> Result<(), ContinueVeto> {
    if autonomy != Autonomy::Continue {
        return Err(ContinueVeto::Policy);
    }
    if !clean_stop {
        return Err(ContinueVeto::UnCleanStop);
    }
    if gate_blocked {
        return Err(ContinueVeto::GateBlocked);
    }
    if continuations_used >= MAX_CONTINUATIONS {
        return Err(ContinueVeto::BudgetSpent);
    }
    Ok(())
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

    /// #001 §6 A0: the safe default. An `Autonomy` nobody stated must be the
    /// one that hands back, never the one that drives itself.
    #[test]
    fn autonomy_defaults_to_the_strictest_mode() {
        assert_eq!(Autonomy::default(), Autonomy::Ask);
    }

    /// The happy path this whole feature exists for: unattended work, a clean
    /// stop, no blocked gate, budget unspent → carry on.
    #[test]
    fn continue_mode_resumes_a_clean_unblocked_turn() {
        assert_eq!(should_continue(Autonomy::Continue, true, false, 0), Ok(()));
    }

    /// The other two modes are handbacks by construction. This is the test
    /// that keeps "持續推進" from silently becoming the behaviour of all three.
    #[test]
    fn review_and_ask_never_continue() {
        for mode in [Autonomy::Review, Autonomy::Ask] {
            assert_eq!(
                should_continue(mode, true, false, 0),
                Err(ContinueVeto::Policy),
                "{mode:?} must hand back"
            );
        }
    }

    /// #001 §6 A1: a turn stopped by a budget (ceiling / loop / deadline /
    /// stuck) already has a graceful exit. Nudging it would fight that exit.
    #[test]
    fn an_unclean_stop_is_never_continued() {
        assert_eq!(
            should_continue(Autonomy::Continue, false, false, 0),
            Err(ContinueVeto::UnCleanStop)
        );
    }

    /// #001 §6 A2 — THE SAFETY BOUNDARY. Continuation and the risk gate are
    /// orthogonal: when a gate blocked, denied or deferred something, the
    /// human is the next step and no autonomy setting may route around them.
    /// If this test ever goes green with `Ok(())`, `Autonomy::Continue` has
    /// become a privilege escalation.
    #[test]
    fn continuation_never_routes_around_a_blocked_gate() {
        assert_eq!(
            should_continue(Autonomy::Continue, true, true, 0),
            Err(ContinueVeto::GateBlocked)
        );
    }

    /// Bounded, and the bound is enforced here rather than by hoping the loop
    /// converges.
    #[test]
    fn continuation_budget_is_spent_after_max() {
        assert_eq!(
            should_continue(Autonomy::Continue, true, false, MAX_CONTINUATIONS),
            Err(ContinueVeto::BudgetSpent)
        );
        assert_eq!(
            should_continue(Autonomy::Continue, true, false, MAX_CONTINUATIONS + 9),
            Err(ContinueVeto::BudgetSpent)
        );
    }

    /// Policy is checked before anything else, so a `Ask` run reports "policy"
    /// rather than leaking why it would ALSO have been stopped.
    #[test]
    fn policy_veto_precedes_every_other_veto() {
        assert_eq!(
            should_continue(Autonomy::Ask, false, true, 99),
            Err(ContinueVeto::Policy)
        );
    }

    #[test]
    fn autonomy_round_trips_as_kebab_case() {
        let y = serde_yaml::to_string(&Autonomy::Continue).unwrap();
        assert!(y.contains("continue"), "got {y}");
        let back: Autonomy = serde_yaml::from_str("review").unwrap();
        assert_eq!(back, Autonomy::Review);
    }

    #[test]
    fn ttl_boundary_is_inclusive_at_seven_days() {
        let now = chrono::Utc::now();
        let exactly = now - chrono::Duration::seconds(APPROVAL_TTL_SECS);
        let over = now - chrono::Duration::seconds(APPROVAL_TTL_SECS + 1);
        assert!(within_approval_ttl(exactly, now));
        assert!(!within_approval_ttl(over, now));
    }
}
