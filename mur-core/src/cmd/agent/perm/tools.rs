//! `mur agent perm` tool policy rules.

use anyhow::Result;
use mur_common::agent::{ToolPolicy, ToolRule};

use super::super::{load_profile_for_edit, save_profile};
use super::warn_if_running;

pub fn cmd_perm_set_tool(name: &str, policy: ToolPolicy, pattern: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let rules = &mut profile.entitlements.tools;
    if let Some(r) = rules.iter_mut().find(|r| r.pattern == pattern) {
        r.policy = policy;
    } else {
        rules.push(ToolRule {
            pattern: pattern.to_string(),
            policy,
            risk: None,
        });
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_clear_tool(name: &str, pattern: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.entitlements.tools.retain(|r| r.pattern != pattern);
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_list_tools(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let rules = &profile.entitlements.tools;
    if rules.is_empty() {
        println!("(no tool rules — all tools use default policy: ask)");
    } else {
        for r in rules {
            println!("{}", rule_line(r));
        }
    }
    Ok(())
}

/// One rule as `perm list-tools` prints it. #1600: a `risk:` above `write`
/// turns `allow` into a prompt, so the line names the EFFECTIVE gate and why
/// — an operator debugging "why does it ask?" reads it here, not in the code.
fn rule_line(r: &mur_common::agent::ToolRule) -> String {
    use mur_common::agent::ToolPolicy;
    let policy = format!("{:?}", r.policy).to_lowercase();
    // #1786: the tool's own tier can lift an allow too; say so on the line.
    let gate = mur_common::hitl::allow_gate::rule_note(r)
        .map(|n| format!("  {}", gate_note(n)))
        .unwrap_or_default();
    let Some(risk) = r.risk else {
        return format!("{policy:10}  {}{gate}", r.pattern);
    };
    let risk_name = serde_json::to_value(risk)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{risk:?}"));
    let note = if r.policy == ToolPolicy::Allow && !mur_common::hitl::tier_may_be_granted(risk) {
        "  → asks (risk above write)"
    } else {
        ""
    };
    format!("{policy:10}  {}  risk={risk_name}{note}{gate}", r.pattern)
}

/// Operator-facing text for an `allow_gate` note. Shared with `doctor`.
pub(crate) fn gate_note(n: mur_common::hitl::allow_gate::RuleNote) -> String {
    use mur_common::hitl::allow_gate::RuleNote;
    let tier = |t: mur_common::hitl::RiskTier| {
        serde_json::to_value(t)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{t:?}"))
    };
    match n {
        RuleNote::Override { intrinsic } => format!(
            "\u{2192} OVERRIDE: runs a {} tool unattended (each call logged)",
            tier(intrinsic)
        ),
        RuleNote::AsksForTool { intrinsic } => format!(
            "\u{2192} asks ({} tool; add `risk: write` on this exact name to run unattended)",
            tier(intrinsic)
        ),
        RuleNote::AsksForSome => {
            "\u{2192} browser actions it matches still ask (network-egress/privileged)".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rule_line;
    use mur_common::agent::{ToolPolicy, ToolRule};
    use mur_common::hitl::RiskTier;

    fn r(policy: ToolPolicy, risk: Option<RiskTier>) -> ToolRule {
        ToolRule {
            pattern: "mcp__browser__*".into(),
            policy,
            risk,
        }
    }

    #[test]
    fn allow_with_high_risk_says_it_asks() {
        assert_eq!(
            rule_line(&r(ToolPolicy::Allow, Some(RiskTier::Destructive))),
            "allow       mcp__browser__*  risk=destructive  → asks (risk above write)"
        );
    }

    #[test]
    fn gate_notes_name_override_and_lifted_allow() {
        let nav = |risk| ToolRule {
            pattern: "mcp__browser__browser_navigate".into(),
            policy: ToolPolicy::Allow,
            risk,
        };
        assert!(rule_line(&nav(Some(RiskTier::Write))).contains("OVERRIDE"));
        assert!(rule_line(&nav(None)).contains("asks (network-egress tool"));
        assert!(rule_line(&r(ToolPolicy::Allow, None)).contains("browser actions"));
    }

    #[test]
    fn low_risk_and_no_risk_add_no_note() {
        let gh = |risk| ToolRule {
            pattern: "mcp__github__*".into(),
            policy: ToolPolicy::Allow,
            risk,
        };
        assert_eq!(
            rule_line(&gh(Some(RiskTier::Write))),
            "allow       mcp__github__*  risk=write"
        );
        assert_eq!(
            rule_line(&r(ToolPolicy::Deny, None)),
            "deny        mcp__browser__*"
        );
        assert_eq!(
            rule_line(&r(ToolPolicy::Deny, Some(RiskTier::Destructive))),
            "deny        mcp__browser__*  risk=destructive",
            "deny already refuses; no note"
        );
    }
}
