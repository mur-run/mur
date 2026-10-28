//! Where one pending call's ask came from, for the `--no-hitl` waiver
//! (`mur_common::hitl::startup_waiver`). Mirrors the arms of
//! [`super::effective_tool_policy`] and [`super::effective_call_policy`] —
//! every arm that asks sets its flag, so an ask may have more than one source.

use mur_common::agent::{ToolPolicy, ToolRule, resolve_tool_policy_opt};
use mur_common::hitl::RiskTier;
use mur_common::hitl::startup_waiver::AskOrigin;
use serde_json::Value;

use super::{builtin_floor, call_floor, declared_tool_risk, policy_name};

/// The origin of an ask for `tool_name` with `input`. `lift` is the call's
/// intent raise; `hook_asked` is a `pre_tool_use` hook's ask.
pub(crate) fn ask_origin(
    rules: &[ToolRule],
    tool_name: &str,
    input: Option<&Value>,
    lift: Option<RiskTier>,
    hook_asked: bool,
) -> AskOrigin {
    let mut origin = AskOrigin {
        intent_raised: lift.is_some(),
        hook_raised: hook_asked,
        ..AskOrigin::default()
    };
    if crate::tools::suggest::suggest_replies_allowed(tool_name) {
        return origin;
    }
    let base = match resolve_tool_policy_opt(rules, tool_name)
        .or_else(|| resolve_tool_policy_opt(rules, policy_name(tool_name)))
    {
        Some(explicit) => {
            origin.explicit = explicit == ToolPolicy::Ask;
            explicit
        }
        None if crate::tools::recall::recall_needs_no_approval(tool_name) => ToolPolicy::Allow,
        None => {
            let d = ToolPolicy::default();
            origin.default = d == ToolPolicy::Ask;
            d
        }
    };
    if base == ToolPolicy::Allow {
        let risk_clamps = declared_tool_risk(rules, tool_name)
            .is_some_and(|t| !mur_common::hitl::tier_may_be_granted(t));
        let unwaived = |f: Option<super::BuiltinFloor>| f.is_some_and(|f| !f.waived);
        origin.clamp = risk_clamps
            || unwaived(builtin_floor(rules, tool_name))
            || unwaived(call_floor(rules, tool_name, input));
    }
    origin
}

#[cfg(test)]
#[path = "ask_origin_tests.rs"]
mod tests;
