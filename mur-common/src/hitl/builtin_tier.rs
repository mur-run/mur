//! Built-in risk tiers that hold no matter what a profile rule says (#1786).
//!
//! The CLI classifier and the runtime gate both read these, so "which browser
//! tools act" is one list. A riskless `allow` rule cannot lower them; only a
//! rule that sets `bypass_builtin_tier_floor: true` opts out, per rule.

use super::RiskTier;

/// Browser MCP tools share one leaf prefix across servers (`mcp__browser__`,
/// `mcp__playwright__`, ...). Matching the LEAF keeps the tier independent of
/// whatever name the operator gave the server.
const BROWSER_PREFIX: &str = "browser_";

/// Browser tools that only observe the current page and hand the result back
/// to the model. Anything not listed here is treated as acting (#1599): the
/// list is an allow-list on purpose, so a tool added upstream fails closed.
const BROWSER_READ_TOOLS: &[&str] = &[
    "browser_snapshot",
    "browser_take_screenshot",
    "browser_screenshot",
    "browser_console_messages",
    "browser_network_requests",
    "browser_generate_locator",
    "browser_wait_for",
];

/// Observers named by family rather than one by one (`browser_verify_text_visible`,
/// `browser_verify_value`, ...): assertions against the page, no side effect.
const BROWSER_READ_PREFIXES: &[&str] = &["browser_verify_"];

/// Browser tools that read or replace the session's cookies and tokens. That
/// is a credential, so it sits with `sudo`, not with a click.
const BROWSER_PRIVILEGED_TOOLS: &[&str] = &["browser_storage_state", "browser_set_storage_state"];

/// #1599: a browser tool drives the operator's logged-in web session — a
/// click can confirm a payment, `evaluate` can POST page data to any host. It
/// used to fall to the `Write` fallback, inside the ceiling, so the default
/// session answered it with no prompt. Acting tools are `NetworkEgress` (every
/// one of them can move data off the machine through the page), which is
/// above `tier_may_be_granted`; observers are `Read`; cookie/token export is
/// `Privileged`. `None` = not a browser tool.
fn classify_browser(leaf: &str) -> Option<RiskTier> {
    if !leaf.starts_with(BROWSER_PREFIX) {
        return None;
    }
    if BROWSER_PRIVILEGED_TOOLS.contains(&leaf) {
        return Some(RiskTier::Privileged);
    }
    if BROWSER_READ_TOOLS.contains(&leaf)
        || BROWSER_READ_PREFIXES.iter().any(|p| leaf.starts_with(p))
    {
        return Some(RiskTier::Read);
    }
    Some(RiskTier::NetworkEgress)
}

/// The built-in tier of a browser tool, matched on the leaf so an MCP server
/// cannot lower it by its prefix. `None` = not a browser tool.
pub fn browser_tier(tool_name: &str) -> Option<RiskTier> {
    classify_browser(tool_name.rsplit("__").next().unwrap_or(tool_name))
}

/// Server-name fragments that mark an MCP server as a browser driver, for
/// [`glob_may_cover_floor`] only. Classification never uses these — it keys
/// on the tool LEAF — so this list only decides which globs get a note.
const BROWSER_SERVER_HINTS: &[&str] = &["browser", "playwright"];

/// Whether an `allow` glob `<prefix>*` can match a tool the floor lifts.
/// Advisory, for `perm` / `doctor` notes: true for the catch-alls, a
/// browser-like server segment, or a leaf stem that can still grow into an
/// acting `browser_*` tool.
pub fn glob_may_cover_floor(prefix: &str) -> bool {
    if prefix.is_empty() || prefix == "mcp__" {
        return true;
    }
    let leaf = match prefix.strip_prefix("mcp__") {
        None => prefix,
        Some(rest) => match rest.split_once("__") {
            Some((_, leaf)) if !leaf.is_empty() => leaf,
            other => {
                let server = other.map_or(rest, |(s, _)| s).to_ascii_lowercase();
                return BROWSER_SERVER_HINTS
                    .iter()
                    .any(|h| server.contains(h) || h.starts_with(server.as_str()));
            }
        },
    };
    BROWSER_PREFIX.starts_with(leaf)
        || (leaf.starts_with(BROWSER_PREFIX)
            && !BROWSER_READ_PREFIXES.iter().any(|p| leaf.starts_with(p)))
}

/// How one profile rule reads under the floor, for operator surfaces
/// (`perm list-tools`, `doctor`). The runtime never uses this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorNote {
    /// Exact-name `allow` + `bypass_builtin_tier_floor`: runs unattended,
    /// logged per call.
    Waived { tier: RiskTier },
    /// Exact-name bare `allow` on a floored tool: asks anyway.
    AsksForTool { tier: RiskTier },
    /// `allow` glob that can match floored tools: those still ask.
    AsksForSome,
    /// `bypass_builtin_tier_floor` on a glob: has no effect.
    IgnoredBypass,
    /// #1814: exact-name `bash: allow` — commands above `ceiling` ask.
    BashCeiling { ceiling: RiskTier },
    /// #1814: `bypass_builtin_tier_floor: true` on `bash` — ignored; name a tier.
    BashTrueIgnored,
}

/// The note `rule` deserves, if any. `allow` rules whose `risk:` already
/// makes them ask are left to #1600's note.
pub fn floor_note(rule: &crate::agent::ToolRule) -> Option<FloorNote> {
    use crate::agent::ToolPolicy;
    if rule.policy != ToolPolicy::Allow || rule.risk.is_some_and(|t| !super::tier_may_be_granted(t))
    {
        return None;
    }
    if rule.pattern == super::floor_waiver::BASH_TOOL {
        use super::floor_waiver::{FloorWaiver, bash_ceiling};
        return Some(match rule.bypass_builtin_tier_floor {
            FloorWaiver::All => FloorNote::BashTrueIgnored,
            _ => FloorNote::BashCeiling {
                ceiling: bash_ceiling(Some(rule)),
            },
        });
    }
    if let Some(prefix) = rule.pattern.strip_suffix('*') {
        if !rule.bypass_builtin_tier_floor.is_off() {
            return Some(FloorNote::IgnoredBypass);
        }
        return glob_may_cover_floor(prefix).then_some(FloorNote::AsksForSome);
    }
    let tier = browser_tier(&rule.pattern).filter(|t| !super::tier_may_be_granted(*t))?;
    Some(if rule.bypass_builtin_tier_floor.covers(tier) {
        FloorNote::Waived { tier }
    } else {
        FloorNote::AsksForTool { tier }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_by_leaf() {
        assert_eq!(
            browser_tier("mcp__browser__browser_click"),
            Some(RiskTier::NetworkEgress)
        );
        assert_eq!(
            browser_tier("mcp__x__browser_snapshot"),
            Some(RiskTier::Read)
        );
        assert_eq!(browser_tier("browser_verify_value"), Some(RiskTier::Read));
        assert_eq!(
            browser_tier("mcp__b__browser_storage_state"),
            Some(RiskTier::Privileged)
        );
        assert_eq!(browser_tier("bash"), None);
        assert_eq!(browser_tier("mcp__browser__navigate"), None);
    }

    #[test]
    fn glob_coverage_flags_catch_alls_and_browser_servers_only() {
        for p in [
            "",
            "mcp__",
            "mcp__browser__",
            "mcp__playwright__",
            "mcp__x__browser_",
            "mcp__x__b",
            "browser_",
            "mcp__play",
        ] {
            assert!(glob_may_cover_floor(p), "{p:?}");
        }
        for p in [
            "mcp__github__",
            "bash",
            "mcp__x__browser_verify_",
            "mcp__x__read_",
            "edit_",
        ] {
            assert!(!glob_may_cover_floor(p), "{p:?}");
        }
    }

    #[test]
    fn floor_notes_cover_waiver_bare_glob_and_ignored_bypass() {
        use crate::agent::{ToolPolicy, ToolRule};
        let r = |pattern: &str, policy, risk, bypass| ToolRule {
            pattern: pattern.into(),
            policy,
            risk,
            bypass_builtin_tier_floor: if bypass {
                crate::hitl::floor_waiver::FloorWaiver::All
            } else {
                crate::hitl::floor_waiver::FloorWaiver::Off
            },
        };
        let nav = "mcp__browser__browser_navigate";
        let egress = RiskTier::NetworkEgress;
        assert_eq!(
            floor_note(&r(nav, ToolPolicy::Allow, None, true)),
            Some(FloorNote::Waived { tier: egress })
        );
        assert_eq!(
            floor_note(&r(nav, ToolPolicy::Allow, None, false)),
            Some(FloorNote::AsksForTool { tier: egress })
        );
        assert_eq!(
            floor_note(&r("mcp__browser__*", ToolPolicy::Allow, None, false)),
            Some(FloorNote::AsksForSome)
        );
        assert_eq!(
            floor_note(&r("mcp__browser__*", ToolPolicy::Allow, None, true)),
            Some(FloorNote::IgnoredBypass)
        );
        for quiet in [
            r("mcp__github__*", ToolPolicy::Allow, None, false),
            r(
                "mcp__browser__browser_snapshot",
                ToolPolicy::Allow,
                None,
                false,
            ),
            r(nav, ToolPolicy::Deny, None, false),
            r(nav, ToolPolicy::Allow, Some(RiskTier::Destructive), true),
        ] {
            assert_eq!(floor_note(&quiet), None, "{}", quiet.pattern);
        }
    }
}
