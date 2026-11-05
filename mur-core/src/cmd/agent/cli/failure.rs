//! A failed tool call, said once (spec 2026-09-29 §4).
//!
//! The runtime hands the CLI a wrapper chain — `tool error: tool execution
//! failed: path not write-entitled: … (grant it via `…`)` — and the card used
//! to print all of it, under a header that already said the call failed. This
//! module reduces it to the two things the operator needs: the innermost
//! cause, and, when the runtime embedded one, the command that fixes it.

/// Wrapper prefixes the transport and the tool layer put in front of the
/// cause. Same list the runtime's ledger strips (`turn_ledger::clean_reason`).
const WRAPPERS: [&str; 2] = ["tool error:", "tool execution failed:"];

/// How the runtime introduces a remediation command inside a cause:
/// `… (grant it via `<command>`)`.
const FIX_LEAD: &str = " (grant it via `";

/// The trailing characters that close [`FIX_LEAD`].
const FIX_TAIL: &str = "`)";

/// One failure, split for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Failure {
    /// The innermost cause, wrapper chain and remediation removed.
    pub cause: String,
    /// The command the runtime said fixes it, when it said one.
    pub fix: Option<String>,
}

/// Split `raw` into its innermost cause and its embedded fix command.
pub(super) fn parse(raw: &str) -> Failure {
    let mut s = raw.trim();
    loop {
        let t = WRAPPERS
            .iter()
            .fold(s, |acc, w| acc.strip_prefix(w).unwrap_or(acc))
            .trim_start();
        if t == s {
            break;
        }
        s = t;
    }
    // The remediation only counts when it closes the cause: a `(grant it via`
    // inside a quoted path is text, not advice.
    if let Some(at) = s.rfind(FIX_LEAD)
        && let Some(cmd) = s[at + FIX_LEAD.len()..].strip_suffix(FIX_TAIL)
        && !cmd.is_empty()
    {
        return Failure {
            cause: s[..at].trim_end().to_string(),
            fix: Some(cmd.to_string()),
        };
    }
    Failure {
        cause: s.to_string(),
        fix: None,
    }
}

/// Indent of the `fix:` row under a failed card's header.
pub(super) const FIX_INDENT: &str = "    ";

#[cfg(test)]
mod tests {
    use super::{Failure, parse};

    #[test]
    fn strips_the_wrapper_chain() {
        assert_eq!(
            parse("tool error: tool execution failed: exit 101"),
            Failure {
                cause: "exit 101".into(),
                fix: None
            }
        );
    }

    #[test]
    fn lifts_the_embedded_fix() {
        let raw = "tool error: tool execution failed: path not write-entitled: \
                   /tmp/x/hello.md (grant it via `mur agent perm allow-write mur /tmp/x/hello.md \
                   && mur agent restart mur`)";
        let f = parse(raw);
        assert_eq!(f.cause, "path not write-entitled: /tmp/x/hello.md");
        assert_eq!(
            f.fix.as_deref(),
            Some("mur agent perm allow-write mur /tmp/x/hello.md && mur agent restart mur")
        );
    }

    #[test]
    fn a_fix_lead_that_does_not_close_the_cause_is_text() {
        let raw = "no such file: a (grant it via `x`)/b";
        assert_eq!(parse(raw).fix, None);
        assert_eq!(parse(raw).cause, raw);
    }

    #[test]
    fn plain_errors_pass_through() {
        assert_eq!(parse("  boom ").cause, "boom");
    }
}
