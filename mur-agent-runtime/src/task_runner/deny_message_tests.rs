//! #940: the deny path stuttered — `tool call denied: denied`.
//! #2581: every refusal tells the model not to route around it.

use super::helpers::{NO_WORKAROUND, approval_required_message, deny_message};

fn denied(suffix: &str) -> String {
    format!("tool call denied{suffix}. {NO_WORKAROUND}")
}

#[test]
fn a_bare_denial_is_not_repeated() {
    // What the CLI sends for a plain `n` press.
    assert_eq!(deny_message(Some("denied")), denied(""));
    assert_eq!(deny_message(Some("  denied  ")), denied(""));
    assert_eq!(deny_message(Some("")), denied(""));
    assert_eq!(deny_message(None), denied(""));
}

#[test]
fn a_reason_that_says_something_survives() {
    assert_eq!(deny_message(Some("timed out")), denied(": timed out"));
    assert_eq!(
        deny_message(Some("no approval channel available")),
        denied(": no approval channel available")
    );
}

/// #2581: every refusal tells the model not to route around it.
#[test]
fn refusals_forbid_a_workaround() {
    assert!(NO_WORKAROUND.starts_with("Do not try to get the same effect"));
    assert!(NO_WORKAROUND.contains("wait for approval or report back"));
    for msg in [
        deny_message(None),
        deny_message(Some("timed out")),
        approval_required_message(None),
        approval_required_message(Some("bash tier destructive")),
    ] {
        assert!(msg.ends_with(NO_WORKAROUND), "{msg}");
    }
}
