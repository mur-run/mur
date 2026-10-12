//! #1786/#1814: what an `allow` rule actually does once the tool's OWN tier
//! is consulted.
//!
//! Before this, `allow` with no `risk:` ran anything unattended — the gate
//! only looked at what the operator declared, never at what the tool is. A
//! profile line `mcp__browser__*: allow` therefore drove the logged-in browser
//! with nobody asked, while the cli's chat gate (which does classify) would
//! have asked for the very same call.
//!
//! The rule now, in order:
//! 1. A declared `risk:` above `Write` asks (#1600, unchanged).
//! 2. A tool whose intrinsic tier is [`needs_attended_allow`] asks — unless
//!    an [`explicit_override`] is on the profile.
//! 3. Everything else runs.
//!
//! Deliberately narrower than "max(declared, intrinsic) > Write": `Spend` and
//! `Destructive` intrinsic tiers are not lifted here. The dispatch tools
//! already carry their own config allowlist, and lifting every tier at once
//! is the "option 2" break that was decided against for existing profiles.
//! Widening the set is a one-line change to [`needs_attended_allow`], made in
//! code with a reason, never in YAML.

use super::RiskTier;
use crate::agent::{ToolPolicy, ToolRule};

/// Intrinsic tiers for which a bare `allow` is not enough: how data leaves
/// the machine (`NetworkEgress`), and credentials/identity (`Privileged`).
pub fn needs_attended_allow(tier: RiskTier) -> bool {
    matches!(tier, RiskTier::NetworkEgress | RiskTier::Privileged)
}

/// The operator's escape hatch, and the ONLY one: a rule naming this exact
/// tool (no glob, no `bash_wait`→`bash` aliasing), with `policy: allow` and
/// `risk: write` written on it. Anything inherited or pattern-matched does
/// not count — an override must be a sentence the operator wrote about this
/// one tool, so it can be found and audited by name.
pub fn explicit_override(rules: &[ToolRule], tool_name: &str) -> bool {
    rules.iter().any(|r| {
        r.pattern == tool_name && r.policy == ToolPolicy::Allow && r.risk == Some(RiskTier::Write)
    })
}

/// How a single profile rule reads under this gate, for operator-facing
/// surfaces (`perm list-tools`, `doctor`). Runtime decisions never use this —
/// they use [`allow_verdict`] on a concrete tool name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleNote {
    /// Exact-name override: runs a gated tool unattended, logged per call.
    Override { intrinsic: RiskTier },
    /// Bare `allow` naming a gated tool exactly: asks anyway.
    AsksForTool { intrinsic: RiskTier },
    /// `allow` glob that can match gated (browser) tools: those ask.
    AsksForSome,
}

/// The note `rule` deserves, if any. Only `allow` rules whose `risk:` does
/// not already make them ask are considered — #1600's note covers those.
pub fn rule_note(rule: &ToolRule) -> Option<RuleNote> {
    use super::tool_name_tier::{classify_by_name, glob_may_cover_gated};
    if rule.policy != ToolPolicy::Allow || rule.risk.is_some_and(|t| !super::tier_may_be_granted(t))
    {
        return None;
    }
    if let Some(prefix) = rule.pattern.strip_suffix('*') {
        return glob_may_cover_gated(prefix).then_some(RuleNote::AsksForSome);
    }
    let intrinsic = classify_by_name(&rule.pattern).filter(|t| needs_attended_allow(*t))?;
    Some(if rule.risk == Some(RiskTier::Write) {
        RuleNote::Override { intrinsic }
    } else {
        RuleNote::AsksForTool { intrinsic }
    })
}

/// What an `allow` that already resolved for `tool_name` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowVerdict {
    /// Runs unattended; nothing about the tool argues otherwise.
    Runs,
    /// Runs unattended ONLY because of an [`explicit_override`] on a tool
    /// whose own tier would have asked. Callers must leave a trace (audit
    /// log line, doctor/perm warning) — this must never be silent.
    RunsByOverride { intrinsic: RiskTier },
    /// Asks first. `intrinsic` is `Some` when the tool's own tier is why.
    Asks { intrinsic: Option<RiskTier> },
}

/// Decide an `allow`. `declared` is the strictest `risk:` matching the tool
/// (however the caller resolves aliases); `intrinsic` is the tool's own tier
/// from a classifier (`None` = the classifier does not decide).
pub fn allow_verdict(
    rules: &[ToolRule],
    tool_name: &str,
    declared: Option<RiskTier>,
    intrinsic: Option<RiskTier>,
) -> AllowVerdict {
    if declared.is_some_and(|t| !super::tier_may_be_granted(t)) {
        return AllowVerdict::Asks { intrinsic: None };
    }
    match intrinsic {
        Some(i) if needs_attended_allow(i) => {
            if explicit_override(rules, tool_name) {
                AllowVerdict::RunsByOverride { intrinsic: i }
            } else {
                AllowVerdict::Asks { intrinsic: Some(i) }
            }
        }
        _ => AllowVerdict::Runs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAV: &str = "mcp__browser__browser_navigate";

    fn rule(pattern: &str, policy: ToolPolicy, risk: Option<RiskTier>) -> ToolRule {
        ToolRule {
            pattern: pattern.into(),
            policy,
            risk,
        }
    }

    fn verdict(rules: &[ToolRule], tool: &str, intrinsic: Option<RiskTier>) -> AllowVerdict {
        let declared = crate::agent::resolve_tool_risk(rules, tool);
        allow_verdict(rules, tool, declared, intrinsic)
    }

    /// Matrix row 1: no `risk:` + a gated intrinsic tier asks.
    #[test]
    fn undeclared_risk_on_a_gated_tool_asks() {
        for tier in [RiskTier::NetworkEgress, RiskTier::Privileged] {
            let rules = [rule("mcp__browser__*", ToolPolicy::Allow, None)];
            assert_eq!(
                verdict(&rules, NAV, Some(tier)),
                AllowVerdict::Asks {
                    intrinsic: Some(tier)
                },
                "{tier:?}"
            );
        }
    }

    /// Matrix row 2: an exact-name `risk: write` runs, and says it is an
    /// override so the caller can audit it.
    #[test]
    fn exact_risk_write_overrides_and_is_marked() {
        let rules = [rule(NAV, ToolPolicy::Allow, Some(RiskTier::Write))];
        assert_eq!(
            verdict(&rules, NAV, Some(RiskTier::NetworkEgress)),
            AllowVerdict::RunsByOverride {
                intrinsic: RiskTier::NetworkEgress
            }
        );
    }

    /// Guardrail 1: a glob is pattern-matched, not a sentence about this tool,
    /// so `risk: write` on it does not open the hatch.
    #[test]
    fn glob_risk_write_is_not_an_override() {
        let rules = [rule(
            "mcp__browser__*",
            ToolPolicy::Allow,
            Some(RiskTier::Write),
        )];
        assert_eq!(
            verdict(&rules, NAV, Some(RiskTier::NetworkEgress)),
            AllowVerdict::Asks {
                intrinsic: Some(RiskTier::NetworkEgress)
            }
        );
    }

    /// Matrix row 5: `risk: read` is not the hatch — declaring a browser
    /// action read-only is a misdeclaration, and max() keeps the higher tier.
    #[test]
    fn declared_read_on_a_gated_tool_still_asks() {
        let rules = [rule(NAV, ToolPolicy::Allow, Some(RiskTier::Read))];
        assert!(matches!(
            verdict(&rules, NAV, Some(RiskTier::NetworkEgress)),
            AllowVerdict::Asks { .. }
        ));
    }

    /// The override cannot undo a higher `risk:` declared by another rule.
    #[test]
    fn override_does_not_beat_a_higher_declared_risk() {
        let rules = [
            rule(NAV, ToolPolicy::Allow, Some(RiskTier::Write)),
            rule(
                "mcp__browser__*",
                ToolPolicy::Allow,
                Some(RiskTier::Destructive),
            ),
        ];
        assert_eq!(
            verdict(&rules, NAV, Some(RiskTier::NetworkEgress)),
            AllowVerdict::Asks { intrinsic: None }
        );
    }

    /// Matrix row 3: an ungated or unclassified tool with no `risk:` runs.
    #[test]
    fn ungated_tools_still_run() {
        let rules = [rule("*", ToolPolicy::Allow, None)];
        for intrinsic in [
            None,
            Some(RiskTier::Read),
            Some(RiskTier::Write),
            Some(RiskTier::Spend),
            Some(RiskTier::Destructive),
        ] {
            assert_eq!(
                verdict(&rules, "edit_file", intrinsic),
                AllowVerdict::Runs,
                "{intrinsic:?} is outside option 3's scope"
            );
        }
    }

    #[test]
    fn rule_notes_cover_override_bare_and_glob() {
        assert_eq!(
            rule_note(&rule(NAV, ToolPolicy::Allow, Some(RiskTier::Write))),
            Some(RuleNote::Override {
                intrinsic: RiskTier::NetworkEgress
            })
        );
        assert_eq!(
            rule_note(&rule(NAV, ToolPolicy::Allow, None)),
            Some(RuleNote::AsksForTool {
                intrinsic: RiskTier::NetworkEgress
            })
        );
        for g in [
            "mcp__browser__*",
            "mcp__playwright__*",
            "*",
            "mcp__x__browser_*",
        ] {
            assert_eq!(
                rule_note(&rule(g, ToolPolicy::Allow, None)),
                Some(RuleNote::AsksForSome),
                "{g}"
            );
        }
        for quiet in [
            rule("mcp__github__*", ToolPolicy::Allow, None),
            rule("edit_file", ToolPolicy::Allow, None),
            rule("mcp__browser__browser_snapshot", ToolPolicy::Allow, None),
            rule(NAV, ToolPolicy::Deny, None),
            rule(NAV, ToolPolicy::Allow, Some(RiskTier::Destructive)),
        ] {
            assert_eq!(rule_note(&quiet), None, "{}", quiet.pattern);
        }
    }

    /// An override line whose policy is not `allow` is not an override.
    #[test]
    fn override_requires_allow_policy() {
        let rules = [rule(NAV, ToolPolicy::Ask, Some(RiskTier::Write))];
        assert!(!explicit_override(&rules, NAV));
    }
}
