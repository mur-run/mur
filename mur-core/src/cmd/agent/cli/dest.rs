//! The CLI's "don't ask again" grant for a gated `bash` call.
//!
//! The tokenizer and destination scope this builds on moved to
//! [`mur_common::hitl::bash_tier::dest`] (#1814) so the agent runtime can use
//! them; they are re-exported here so callers keep the `dest::` path. What
//! stays is [`Grant`], which needs [`super::egress`] (and through it `git2`).

pub(super) use mur_common::hitl::bash_tier::dest::*;

/// What "don't ask again" would actually buy for one gated call.
///
/// The modal used to offer that row unconditionally and key it on the tool
/// name, which produced the bug this module exists for: the operator picked
/// it, `session_tool_allow` recorded `bash`, and the next call was asked
/// anyway because `tier_may_be_granted(NetworkEgress)` is false and the set
/// was never consulted. The intent was dropped in silence.
///
/// So the row's meaning is computed ONCE, here, and both the menu (which
/// prints it) and the auto lane (which honours it) read the same answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Grant {
    /// Grantable, keyed on a destination proved read-only.
    Scoped(Scope),
    /// Grantable, keyed on an egress action and its resolved destination
    /// (`egress::classify`). Session-only: a human answered for this call.
    Egress(super::egress::EgressScope),
    /// Grantable, keyed on the tool name — the pre-existing behaviour, still
    /// bounded by `tier_may_be_granted`.
    Tool(String),
    /// NOT grantable. Carries the reason so the row can say it out loud
    /// instead of pretending.
    Refused(&'static str),
}

impl Grant {
    /// The `session_tool_allow` key, or `None` when nothing may be stored.
    pub fn key(&self) -> Option<String> {
        match self {
            Self::Scoped(s) => Some(s.key()),
            Self::Egress(s) => Some(s.key()),
            Self::Tool(t) => Some(t.clone()),
            Self::Refused(_) => None,
        }
    }

    /// The menu-row wording.
    pub fn label(&self) -> String {
        match self {
            Self::Scoped(s) => s.label(),
            Self::Egress(s) => s.label(),
            Self::Tool(t) => format!("Yes, and don't ask again for `{t}` this session"),
            Self::Refused(why) => format!("Yes (can't skip future asks — {why})"),
        }
    }
}

/// Decide what a session grant may cover for this call.
///
/// The order matters. A destination scope is tried FIRST, because its proof is
/// stronger than the tier check it would otherwise fall to: `ssh host tail -f
/// log` is `NetworkEgress` by head, but a parsed, one-hop, read-only-at-both-
/// ends command is a narrower thing than that tier describes, and refusing to
/// remember it is what trained the operator to answer without reading.
pub(super) fn grant_for(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    tier: mur_common::hitl::RiskTier,
) -> Grant {
    if tool_name == "bash"
        && let Some(cmd) = tool_input
            .and_then(|v| v.get("command"))
            .and_then(serde_json::Value::as_str)
    {
        if let Some(scope) = classify_bash(cmd) {
            return Grant::Scoped(scope);
        }
        // An egress scope is a SESSION answer only. It is not a tier grant:
        // `tier_may_be_granted` is untouched, so no config line, fleet grant
        // or `/auto` session reaches it — only a human's "don't ask again".
        if let Some(scope) = super::egress::classify(cmd) {
            return Grant::Egress(scope);
        }
        if !mur_common::hitl::tier_may_be_granted(tier) {
            return Grant::Refused(refusal_reason(tier));
        }
    }
    if mur_common::hitl::tier_may_be_granted(tier) {
        return Grant::Tool(tool_name.to_string());
    }
    Grant::Refused("this tier always asks")
}

/// Why a tier above the ceiling cannot be remembered, in words that name
/// that tier. It used to be one sentence for all four — "this command writes
/// or leaves the machine" — which told the operator a `sudo` could leak data
/// and a `git push` could delete it, and so told them nothing.
fn refusal_reason(tier: mur_common::hitl::RiskTier) -> &'static str {
    use mur_common::hitl::RiskTier;
    match tier {
        RiskTier::NetworkEgress => "this command sends data off this machine",
        RiskTier::Destructive => "this command can delete or overwrite data",
        RiskTier::Privileged => "this command runs with elevated privileges",
        RiskTier::Spend => "this command can spend money",
        // Unreachable behind `tier_may_be_granted`, but a wording must exist.
        RiskTier::Read | RiskTier::Write => "this tier always asks",
    }
}

#[cfg(test)]
mod tests;
