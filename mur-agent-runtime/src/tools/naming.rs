//! MCP wire-name encoding: `mcp__<server>__<tool>`.
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
    fn wire_name_with_sanitized() {
        let s = sanitize_server("my/server");
        assert_eq!(wire_name(&s, "do_thing"), "mcp__my_server__do_thing");
    }
}
