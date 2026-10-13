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
            bypass_builtin_tier_floor: Default::default(),
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
    // #1786: the tool's built-in tier can lift an allow too; say so.
    let floor = mur_common::hitl::builtin_tier::floor_note(r)
        .map(|n| format!("  {}", floor_note_text(n)))
        .unwrap_or_default();
    let Some(risk) = r.risk else {
        return format!("{policy:10}  {}{floor}", r.pattern);
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
    format!("{policy:10}  {}  risk={risk_name}{note}{floor}", r.pattern)
}

/// Operator-facing text for a built-in tier floor note. Shared with `doctor`.
pub(crate) fn floor_note_text(n: mur_common::hitl::builtin_tier::FloorNote) -> String {
    use mur_common::hitl::builtin_tier::FloorNote;
    let tier = |t: mur_common::hitl::RiskTier| {
        serde_json::to_value(t)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{t:?}"))
    };
    match n {
        FloorNote::Waived { tier: t } => format!(
            "\u{2192} FLOOR WAIVED: runs a {} tool unattended (each call logged)",
            tier(t)
        ),
        FloorNote::AsksForTool { tier: t } => format!(
            "\u{2192} asks ({} tool; set bypass_builtin_tier_floor on this rule to run unattended)",
            tier(t)
        ),
        FloorNote::AsksForSome => {
            "\u{2192} acting browser tools it matches still ask (built-in tier)".to_string()
        }
        FloorNote::BashCeiling { ceiling: c } => format!(
            "\u{2192} commands above {} ask (bash tier ceiling, #1814)",
            tier(c)
        ),
        FloorNote::BashTrueIgnored => {
            "\u{2192} bypass_builtin_tier_floor: true IGNORED on bash; name a tier, e.g. network-egress"
                .to_string()
        }
        FloorNote::IgnoredBypass => {
            "\u{2192} bypass_builtin_tier_floor IGNORED: a glob cannot waive the floor; name each tool"
                .to_string()
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
            bypass_builtin_tier_floor: Default::default(),
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
    fn floor_notes_name_waiver_lift_and_ignored_bypass() {
        let nav = |bypass| ToolRule {
            pattern: "mcp__browser__browser_navigate".into(),
            policy: ToolPolicy::Allow,
            risk: None,
            bypass_builtin_tier_floor: if bypass {
                mur_common::hitl::floor_waiver::FloorWaiver::All
            } else {
                mur_common::hitl::floor_waiver::FloorWaiver::Off
            },
        };
        assert!(rule_line(&nav(true)).contains("FLOOR WAIVED"));
        assert!(rule_line(&nav(false)).contains("asks (network-egress tool"));
        assert!(rule_line(&r(ToolPolicy::Allow, None)).contains("acting browser tools"));
        let mut glob = r(ToolPolicy::Allow, None);
        glob.bypass_builtin_tier_floor = mur_common::hitl::floor_waiver::FloorWaiver::All;
        assert!(rule_line(&glob).contains("IGNORED"));
    }

    #[test]
    fn bash_rule_names_its_ceiling_or_the_ignored_true() {
        use mur_common::hitl::floor_waiver::FloorWaiver;
        let bash = |w| ToolRule {
            pattern: "bash".into(),
            policy: ToolPolicy::Allow,
            risk: None,
            bypass_builtin_tier_floor: w,
        };
        assert!(
            rule_line(&bash(FloorWaiver::Off)).contains("above write ask"),
            "{}",
            rule_line(&bash(FloorWaiver::Off))
        );
        assert!(
            rule_line(&bash(FloorWaiver::UpTo(RiskTier::NetworkEgress)))
                .contains("above network-egress")
        );
        assert!(rule_line(&bash(FloorWaiver::All)).contains("true IGNORED on bash"));
    }

    #[test]
    fn low_risk_and_no_risk_add_no_note() {
        let gh = ToolRule {
            pattern: "mcp__github__*".into(),
            policy: ToolPolicy::Allow,
            risk: Some(RiskTier::Write),
            bypass_builtin_tier_floor: Default::default(),
        };
        assert_eq!(rule_line(&gh), "allow       mcp__github__*  risk=write");
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
