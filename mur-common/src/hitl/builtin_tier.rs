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
}
