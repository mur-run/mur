//! MCP wire-name encoding: `mcp__<server>__<tool>`.
//!
//! Single source of truth for the wire-name contract shared by the agent
//! runtime (tool registry) and mur-core (per-tool policy patterns).
//!
//! The LLM tool-name field must match `^[a-zA-Z0-9_-]{1,64}$`.
//! Server names are sanitised by collapsing non-alphanumeric/dash chars into `_`.

/// Sanitise an MCP server name so it's safe to embed in a wire name.
///
/// Collapses any run of non-`[a-zA-Z0-9-]` chars into a single `_`.
pub fn sanitize_server(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_us = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    // Trim trailing underscore that would make the boundary look odd.
    out.trim_end_matches('_').to_string()
}

/// Encode a server + tool name into the `mcp__<server>__<tool>` wire format.
pub fn wire_name(server_sanitized: &str, tool: &str) -> String {
    format!("mcp__{server_sanitized}__{tool}")
}

/// Prefix-glob `ToolRule` pattern matching every tool of `server`
/// (sanitises first): `mcp__<server>__*`. Feed to
/// `mur_common::agent::resolve_tool_policy` / `ToolRule.pattern`.
pub fn tool_pattern(server: &str) -> String {
    format!("mcp__{}__*", sanitize_server(server))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_normal() {
        assert_eq!(sanitize_server("github"), "github");
    }

    #[test]
    fn sanitize_slash_to_underscore() {
        assert_eq!(sanitize_server("my/server"), "my_server");
    }

    #[test]
    fn sanitize_collapse_runs() {
        assert_eq!(sanitize_server("my//server"), "my_server");
    }

    #[test]
    fn sanitize_dash_preserved() {
        assert_eq!(sanitize_server("my-server"), "my-server");
    }

    #[test]
    fn wire_name_format() {
        assert_eq!(wire_name("github", "merge_pr"), "mcp__github__merge_pr");
    }

    #[test]
    fn tool_pattern_matches_wire_names_of_that_server() {
        use crate::agent::{ToolPolicy, ToolRule, resolve_tool_policy};
        let rules = vec![ToolRule {
            pattern: tool_pattern("research-gateway"),
            policy: ToolPolicy::Allow,
            risk: None,
        }];
        // Every tool of the server resolves to Allow…
        let wn = wire_name(&sanitize_server("research-gateway"), "research_search");
        assert_eq!(resolve_tool_policy(&rules, &wn), ToolPolicy::Allow);
        // …other servers' tools stay at the default (Ask).
        let other = wire_name("github", "merge_pr");
        assert_eq!(resolve_tool_policy(&rules, &other), ToolPolicy::Ask);
    }
}
