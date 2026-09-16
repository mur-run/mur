//! Risk classification for monitor actions (spec §風險政策).
//!
//! A fixed match on the action *type*. Never on its parameters, never on
//! anything an agent wrote, and never from a model:
//! `mur_common::hitl::RiskTier`'s own doc says "Tier is resolved
//! most-restrictive-wins and is NEVER LLM-asserted", and the spec says
//! 「agent 不得靠改寫動作名稱繞過分類」.
//!
//! The fallback is `Privileged`, not `Read`. A verb this table does not
//! name is a verb nobody classified, and the failure mode of guessing low
//! is an unattended process doing something nobody approved.

use mur_common::hitl::RiskTier;

pub fn classify(action_type: &str) -> RiskTier {
    match action_type {
        // Local, no external write, no new credential.
        "notify" => RiskTier::Read,
        "collect_logs" => RiskTier::Read,
        "reschedule_monitor" => RiskTier::Read,
        // Writes to an external system. spec §風險政策: a rerun is only
        // low-risk for a job explicitly marked flaky and under its cap —
        // a condition this slice has no way to establish, so it asks.
        "rerun" => RiskTier::Write,
        // spec §風險政策 names this one: being written under on_success
        // does not make deploying production low-risk.
        "start_downstream" => RiskTier::Privileged,
        // No remedy catalogue exists; anything claiming to apply one is
        // unclassifiable by definition.
        "apply_known_remedy" => RiskTier::Privileged,
        _ => RiskTier::Privileged,
    }
}
