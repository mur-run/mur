//! The one spelling of "you may not" that every authorization gate uses
//! (spec 2026-09-12 execution-limits §3.8, D9). The gates stay where they
//! are; this is how their refusals are recognised across a process boundary
//! — an MCP server's `isError` text, a tool's error — so the loop can treat
//! them as terminal for the tool instead of as something to retry.

pub const NOT_AUTHORIZED_PREFIX: &str = "not authorized:";

/// `not authorized: <msg>` — the message every gate emits.
pub fn not_authorized(msg: &str) -> String {
    format!("{NOT_AUTHORIZED_PREFIX} {msg}")
}

/// Does this text (possibly wrapped by an MCP server as `Error: …`) carry a
/// refusal? Prefix only — a tool that merely mentions authorization in its
/// output is not refusing.
pub fn is_not_authorized(text: &str) -> bool {
    let t = text.trim_start();
    let t = t.strip_prefix("Error:").map(str::trim_start).unwrap_or(t);
    t.len() >= NOT_AUTHORIZED_PREFIX.len()
        && t[..NOT_AUTHORIZED_PREFIX.len()].eq_ignore_ascii_case(NOT_AUTHORIZED_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_are_recognised_with_or_without_the_mcp_wrapper() {
        assert!(is_not_authorized(&not_authorized(
            "target 'ghost' for parallel_jobs"
        )));
        assert!(is_not_authorized("Error: not authorized: fleet_run denied"));
        assert!(is_not_authorized("  NOT AUTHORIZED: x"));
        assert!(
            !is_not_authorized("the file says: not authorized: nope"),
            "prefix, not substring"
        );
        assert!(!is_not_authorized("permission denied"));
    }
}
