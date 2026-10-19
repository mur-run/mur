//! #1997: the reason text for a call refused because the caller cannot
//! approve it.
//!
//! The old text always said "allow the tool with `tool-allow`" — wrong when an
//! `allow` rule already matched and the call asked anyway (the bash tier
//! ceiling, a declared `risk:`, a built-in tier floor). It also said "run it
//! from `murmur`", but `!mur agent send …` inside murmur has stdin = null and
//! fails the same way; only a chat opened in its own terminal can answer.

use mur_common::agent::{ToolPolicy, ToolRule, resolve_tool_policy_opt};
use mur_common::hitl::RiskTier;
use mur_common::hitl::floor_waiver::{BASH_WAIVER_MAX, CeilingHit, bash_ceiling_hit};

/// Placeholder used when the gate does not know its own agent name.
const AGENT_PLACEHOLDER: &str = "<agent>";

/// Why an `Ask` call needed a human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AskCause {
    /// No `allow` rule matched: allowing the tool is a real way out.
    NoAllowRule,
    /// An `allow` rule matched, but the `bash` command is above the ceiling.
    BashCeiling(CeilingHit),
    /// An `allow` rule matched, but a declared or built-in tier still asks.
    AllowedButTiered,
}

/// Classify why `tool_name` with `input` asked, from the same rules the gate
/// used. Only meaningful for a call whose effective policy was `Ask`.
pub(crate) fn ask_cause(
    rules: &[ToolRule],
    tool_name: &str,
    input: Option<&serde_json::Value>,
) -> AskCause {
    if super::effective_tool_policy(rules, tool_name) == ToolPolicy::Allow
        && let Some(hit) = bash_ceiling_hit(rules, tool_name, input)
    {
        return AskCause::BashCeiling(hit);
    }
    let explicit = resolve_tool_policy_opt(rules, tool_name)
        .or_else(|| resolve_tool_policy_opt(rules, super::policy_name(tool_name)));
    if explicit == Some(ToolPolicy::Allow) {
        AskCause::AllowedButTiered
    } else {
        AskCause::NoAllowRule
    }
}

fn tier_name(t: RiskTier) -> String {
    // The wire/YAML spelling (`network-egress`), so the hint can be pasted.
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{t:?}"))
}

/// Where an approval can actually be given from a non-interactive caller.
fn approve_hint(agent: &str) -> String {
    format!(
        "approve it from a chat opened in its own terminal (`mur agent cli {agent} --ask`; \
         `!` commands inside murmur cannot answer a prompt)"
    )
}

/// The refusal text for a caller that cannot approve.
pub(crate) fn cannot_approve_reason(agent: &str, tool_name: &str, cause: &AskCause) -> String {
    let agent = if agent.is_empty() {
        AGENT_PLACEHOLDER
    } else {
        agent
    };
    let approve = approve_hint(agent);
    match cause {
        AskCause::NoAllowRule => format!(
            "`{tool_name}` needs approval and this caller cannot give one — {approve}, \
             or allow the tool with `mur agent perm tool-allow {agent} {tool_name}`"
        ),
        AskCause::BashCeiling(hit) => {
            let tier = tier_name(hit.tier);
            let ceiling = tier_name(hit.ceiling);
            let raise = if hit.tier <= BASH_WAIVER_MAX {
                format!(
                    ", or raise the ceiling with `bypass_builtin_tier_floor: {tier}` on the \
                     exact `{tool_name}` rule in profile.yaml and `mur agent perm reseal {agent}`"
                )
            } else {
                format!(
                    "; tier `{tier}` is above the highest waivable ceiling `{}`, so it always \
                     needs a human",
                    tier_name(BASH_WAIVER_MAX)
                )
            };
            format!(
                "`{tool_name}` command is tier `{tier}`, above this agent's bash ceiling \
                 `{ceiling}`, and this caller cannot approve it — {approve}{raise}"
            )
        }
        AskCause::AllowedButTiered => format!(
            "`{tool_name}` is already allowed but its risk tier still needs approval, and this \
             caller cannot give one — {approve}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::hitl::floor_waiver::FloorWaiver;

    fn bash_allow(waiver: FloorWaiver) -> Vec<ToolRule> {
        vec![ToolRule {
            pattern: "bash".into(),
            policy: ToolPolicy::Allow,
            risk: None,
            bypass_builtin_tier_floor: waiver,
        }]
    }

    fn cmd(c: &str) -> serde_json::Value {
        serde_json::json!({ "command": c })
    }

    /// (a) No allow rule: `tool-allow` is a real way out and stays.
    #[test]
    fn no_allow_rule_keeps_the_tool_allow_hint() {
        let cause = ask_cause(&[], "bash", Some(&cmd("ls")));
        assert_eq!(cause, AskCause::NoAllowRule);
        let why = cannot_approve_reason("reviewer", "bash", &cause);
        assert!(
            why.contains("mur agent perm tool-allow reviewer bash"),
            "{why}"
        );
        assert!(why.contains("own terminal"), "{why}");
        assert!(!why.contains("from `murmur`"), "{why}");
    }

    /// (b) The issue's repro: `bash: allow`, `gh pr view` is network-egress.
    #[test]
    fn ceiling_hit_names_tier_and_ceiling_and_drops_tool_allow() {
        let rules = bash_allow(FloorWaiver::Off);
        let cause = ask_cause(&rules, "bash", Some(&cmd("git push origin main")));
        let AskCause::BashCeiling(hit) = &cause else {
            panic!("expected a ceiling hit, got {cause:?}");
        };
        assert_eq!(hit.ceiling, RiskTier::Write);
        let why = cannot_approve_reason("reviewer", "bash", &cause);
        assert!(why.contains("tier `network-egress`"), "{why}");
        assert!(why.contains("bash ceiling `write`"), "{why}");
        assert!(!why.contains("tool-allow"), "{why}");
        assert!(
            why.contains("bypass_builtin_tier_floor: network-egress"),
            "{why}"
        );
        assert!(why.contains("mur agent perm reseal reviewer"), "{why}");
        assert!(why.contains("mur agent cli reviewer --ask"), "{why}");
    }

    /// Never suggest a waiver above `BASH_WAIVER_MAX`.
    #[test]
    fn privileged_is_never_offered_as_a_ceiling() {
        let rules = bash_allow(FloorWaiver::UpTo(RiskTier::Destructive));
        let cause = ask_cause(&rules, "bash", Some(&cmd("sudo launchctl list")));
        let why = cannot_approve_reason("reviewer", "bash", &cause);
        assert!(why.contains("tier `privileged`"), "{why}");
        assert!(!why.contains("bypass_builtin_tier_floor"), "{why}");
        assert!(why.contains("always needs a human"), "{why}");
    }

    /// An `allow` rule that matched but asked for another reason (declared
    /// `risk:`) must not be told to `tool-allow` either.
    #[test]
    fn allowed_but_tiered_drops_tool_allow() {
        let rules = vec![ToolRule {
            pattern: "drop_db".into(),
            policy: ToolPolicy::Allow,
            risk: Some(RiskTier::Destructive),
            bypass_builtin_tier_floor: FloorWaiver::Off,
        }];
        let cause = ask_cause(&rules, "drop_db", None);
        assert_eq!(cause, AskCause::AllowedButTiered);
        let why = cannot_approve_reason("a", "drop_db", &cause);
        assert!(!why.contains("tool-allow"), "{why}");
        assert!(why.contains("already allowed"), "{why}");
    }

    #[test]
    fn unknown_agent_falls_back_to_a_placeholder() {
        let why = cannot_approve_reason("", "remember", &AskCause::NoAllowRule);
        assert!(why.contains("tool-allow <agent> remember"), "{why}");
    }
}
