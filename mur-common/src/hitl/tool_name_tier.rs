//! The tier a tool call has by NAME alone — no arguments consulted.
//!
//! Lives below both `mur-core` (the cli's chat gate, which adds the
//! argument-level bash classifier on top) and `mur-agent-runtime` (gate B,
//! which must not depend on `mur-core`). Two gates, one table: if each kept
//! its own copy, a browser tool added to one list and not the other would be
//! a prompt on one surface and silent on the other.
//!
//! `None` from [`classify_by_name`] means "the name alone does not decide" —
//! the caller falls back to its own rule (the cli's is `Write`).

use super::RiskTier;

/// Tool names that are spend/dispatch by nature, whatever their arguments.
/// Mirrors `NO_ALWAYS_TOOLS` in the GUI's `hitlModel.ts` — one list of
/// "never standing-granted" dispatch tools, two surfaces, and they must not
/// drift.
const SPEND_TOOLS: &[&str] = &["fleet_run", "parallel_jobs", "delegate_to"];

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

/// The tier `tool_name` has regardless of its arguments, or `None` when the
/// name alone does not decide. Never LLM-asserted.
pub fn classify_by_name(tool_name: &str) -> Option<RiskTier> {
    let leaf = leaf(tool_name);
    if SPEND_TOOLS.contains(&leaf) {
        return Some(RiskTier::Spend);
    }
    classify_browser(leaf)
}

/// #1599: a browser tool drives the operator's logged-in web session — a
/// click can confirm a payment, `evaluate` can POST page data to any host.
/// Acting tools are `NetworkEgress` (every one of them can move data off the
/// machine through the page), which is above `tier_may_be_granted`; observers
/// are `Read`; cookie/token export is `Privileged`. `None` = not a browser
/// tool.
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

/// Server-name fragments that mark an MCP server as a browser driver, for
/// [`glob_may_cover_gated`] only. Classification itself never uses these —
/// it keys on the tool LEAF, so an unlisted server still classifies right;
/// this list only decides which globs get an operator-facing note.
const BROWSER_SERVER_HINTS: &[&str] = &["browser", "playwright"];

/// Whether an `allow` glob `<prefix>*` can match a tool whose name tier needs
/// an attended allow. Advisory (for `perm` / `doctor` notes): `true` for the
/// catch-alls, a browser-like server segment, or a leaf that can still grow
/// into `browser_<acting>`.
pub fn glob_may_cover_gated(prefix: &str) -> bool {
    if prefix.is_empty() || prefix == "mcp__" {
        return true;
    }
    // A non-MCP glob is all leaf: `browser_*` matches a bare `browser_click`.
    let leaf = match prefix.strip_prefix("mcp__") {
        None => prefix,
        Some(rest) => match rest.split_once("__") {
            Some((_, leaf)) if !leaf.is_empty() => leaf,
            // Glob ends inside or right after the server segment: any tool
            // of a browser-like server (or one it could still grow into).
            other => {
                let server = other.map_or(rest, |(s, _)| s).to_ascii_lowercase();
                return BROWSER_SERVER_HINTS
                    .iter()
                    .any(|h| server.contains(h) || h.starts_with(server.as_str()));
            }
        },
    };
    // `browser_` or any shorter stem can grow into an acting tool; a longer
    // one can too, unless it sits inside an observer-only family.
    BROWSER_PREFIX.starts_with(leaf)
        || (leaf.starts_with(BROWSER_PREFIX)
            && !BROWSER_READ_PREFIXES.iter().any(|p| leaf.starts_with(p)))
}

/// `mcp__server__tool` → `tool`. An MCP server must not lower a tool's tier by
/// prefixing it.
fn leaf(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hitl::tier_may_be_granted;

    #[test]
    fn glob_coverage_flags_catch_alls_and_browser_servers_only() {
        for p in [
            "",
            "mcp__",
            "mcp__browser__",
            "mcp__playwright__",
            "mcp__x__browser_",
            "mcp__x__b",
            "mcp__x__browser_cli",
            "browser_",
            "mcp__play",
        ] {
            assert!(glob_may_cover_gated(p), "{p:?}");
        }
        for p in [
            "mcp__github__",
            "bash",
            "mcp__x__browser_verify_",
            "mcp__x__read_",
            "edit_",
        ] {
            assert!(!glob_may_cover_gated(p), "{p:?}");
        }
    }

    /// Dispatch tools are Spend by name, and an MCP prefix must not launder them.
    #[test]
    fn dispatch_tools_are_spend_under_any_prefix() {
        assert_eq!(classify_by_name("fleet_run"), Some(RiskTier::Spend));
        assert_eq!(classify_by_name("parallel_jobs"), Some(RiskTier::Spend));
        assert_eq!(
            classify_by_name("mcp__media__parallel_jobs"),
            Some(RiskTier::Spend),
            "an MCP server prefix must not lower a tool's tier"
        );
    }

    /// #1599: every acting browser tool lands above the grant ceiling under
    /// any MCP server name, an unknown future `browser_*` tool fails closed,
    /// and only the listed observers are Read.
    #[test]
    fn browser_tools_that_act_are_above_the_ceiling() {
        for tool in [
            "mcp__browser__browser_navigate",
            "mcp__playwright__browser_click",
            "mcp__playwright__browser_fill_form",
            "mcp__browser__browser_evaluate",
            "mcp__anything__browser_file_upload",
            "mcp__browser__browser_some_future_action",
            "browser_type",
        ] {
            let t = classify_by_name(tool).expect("a browser tool");
            assert!(
                !tier_may_be_granted(t),
                "{tool} -> {t:?} must not be grantable"
            );
        }
        assert_eq!(
            classify_by_name("mcp__browser__browser_storage_state"),
            Some(RiskTier::Privileged),
            "exporting cookies/tokens is a credential action"
        );
        for tool in [
            "mcp__browser__browser_snapshot",
            "mcp__playwright__browser_take_screenshot",
            "mcp__browser__browser_verify_text_visible",
        ] {
            assert_eq!(classify_by_name(tool), Some(RiskTier::Read), "{tool}");
        }
    }

    /// The name alone does not decide for anything else — bash, edit tools,
    /// and non-browser MCP tools are left to the caller.
    #[test]
    fn other_names_are_undecided() {
        for tool in ["bash", "edit_file", "mcp__notes__search", "recall"] {
            assert_eq!(classify_by_name(tool), None, "{tool}");
        }
    }
}
