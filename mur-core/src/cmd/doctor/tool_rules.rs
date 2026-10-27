//! #2147: tool rules whose pattern differs from a real tool only by case.

use super::Check;

/// Matching is exact, so a miscased rule does nothing — and a `deny`/`ask`/
/// `risk:` rule that does nothing fails OPEN. MCP tool lists are not known
/// offline, so an MCP pattern is only checked up to its `mcp__<server>__`
/// prefix.
pub(crate) fn check_tool_rule_case(profile: &mur_common::AgentProfile) -> Check {
    let known = mur_common::agent::KnownTools::for_servers(
        profile.mcp_servers.iter().map(|s| s.name.as_str()),
    );
    let dead = mur_common::agent::dead_rules(&profile.entitlements.tools, &known);
    if dead.is_empty() {
        return Check::new("tool_rules", true, "no case-misspelled tool rules");
    }
    Check::new(
        "tool_rules",
        false,
        dead.iter()
            .map(|d| d.message())
            .collect::<Vec<_>>()
            .join("; "),
    )
}
