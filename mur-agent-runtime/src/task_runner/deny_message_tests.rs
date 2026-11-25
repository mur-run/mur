//! #940: the deny path stuttered — `tool call denied: denied`.
//! #2581: every refusal tells the model not to route around it.

use super::{NO_WORKAROUND, approval_required_message, deny_message};

#[test]
fn a_bare_denial_is_not_repeated() {
    // What the CLI sends for a plain `n` press.
    let bare = format!("tool call denied; {NO_WORKAROUND}");
    assert_eq!(deny_message(Some("denied")), bare);
    assert_eq!(deny_message(Some("  denied  ")), bare);
    assert_eq!(deny_message(Some("")), bare);
    assert_eq!(deny_message(None), bare);
}

#[test]
fn a_reason_that_says_something_survives() {
    assert_eq!(
        deny_message(Some("timed out")),
        format!("tool call denied: timed out; {NO_WORKAROUND}")
    );
    assert_eq!(
        deny_message(Some("no approval channel available")),
        format!("tool call denied: no approval channel available; {NO_WORKAROUND}")
    );
}

/// The wording is the contract: a model that reads either refusal is
/// told not to reach the same effect another way.
#[test]
fn refusals_forbid_routing_around_the_gate() {
    assert_eq!(
        NO_WORKAROUND,
        "do not get the same effect through another tool, command or language — \
         wait for the approval, or report back that it is needed"
    );
    for msg in [
        approval_required_message(Some("tier `destructive`")),
        approval_required_message(None),
        deny_message(None),
        deny_message(Some("timed out")),
    ] {
        assert!(msg.ends_with(NO_WORKAROUND), "{msg}");
    }
}
