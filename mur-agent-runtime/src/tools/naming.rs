//! MCP wire-name encoding — re-exported from `mur_common::mcp_naming`
//! (single source of truth; mur-core builds `ToolRule` patterns from the
//! same contract).

pub use mur_common::mcp_naming::{sanitize_server, wire_name};

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
