//! `show`'s unblock hint once the monitor itself has given up.
//!
//! Its own file, in the sibling-submodule layout `monitor_tests.rs` already
//! uses for `write_grant.rs` — the parent is 782 lines and these would put
//! it over CLAUDE.md's 800-line rule.

use super::*;

/// `home_with_blocked_action`, then the monitor pushed to `exhausted` — the
/// state a spent remediation budget leaves behind while another gated action
/// is still parked. Reachable with `on_failure: [rerun, start_downstream]`
/// and a cap of 1: the approved `rerun` fails, spends the only attempt, and
/// the second action never leaves `blocked`.
fn blocked_action_on_an_exhausted_monitor(
    verb: &str,
    hitl_id: &str,
) -> (tempfile::TempDir, String) {
    let (d, id) = home_with_blocked_action(verb, hitl_id);
    let s = MonitorStore::open(d.path()).unwrap();
    s.set_state(&id, MonitorState::Exhausted, t0()).unwrap();
    (d, id)
}

fn blocked_line(out: &str, verb: &str) -> String {
    out.lines()
        .find(|l| l.contains(verb) && l.contains("blocked"))
        .unwrap_or_else(|| panic!("no blocked {verb} line in:\n{out}"))
        .to_string()
}

#[test]
fn an_exhausted_monitor_says_approving_alone_will_not_restart_it() {
    // The bug: the hint matched only on the ACTION's state, never the
    // monitor's. Once a monitor is `exhausted` the drain skips every one of
    // its actions, so the printed `approve` command succeeds — the approval
    // really does land on the channel — and then nothing acts on it, ever,
    // with no error. We were sending the user to a command that works and
    // achieves nothing: the same silent stop this line exists to prevent,
    // one step further along.
    let (d, id) = blocked_action_on_an_exhausted_monitor("rerun", "hitl-exh");
    let out = go(
        d.path(),
        MonitorAction::Show {
            id: id.clone(),
            history: false,
        },
    )
    .unwrap();
    let line = blocked_line(&out, "rerun");

    // Both commands. Asserting only on `retry` would pass an implementation
    // that dropped the approve command the user still has to run first.
    assert!(line.contains("mur channel approve"), "{line}");
    assert!(line.contains("hitl-exh"), "{line}");
    assert!(line.contains("mur monitor retry"), "{line}");
    assert!(
        line.contains("will not restart it"),
        "must say why approving alone is not enough: {line}"
    );
    assert!(
        line.find("mur channel approve").unwrap() < line.find("mur monitor retry").unwrap(),
        "printed in the order they must be run: {line}"
    );
}

#[test]
fn a_live_monitor_still_prints_only_the_approve_command() {
    // The control. Without it, an implementation that appended the retry
    // hint to EVERY blocked action would pass the test above — while telling
    // users to retry monitors that never gave up.
    let (d, id) = home_with_blocked_action("rerun", "hitl-live");
    let out = go(
        d.path(),
        MonitorAction::Show {
            id: id.clone(),
            history: false,
        },
    )
    .unwrap();
    let line = blocked_line(&out, "rerun");
    assert!(line.contains("mur channel approve"), "{line}");
    assert!(
        !line.contains("mur monitor retry"),
        "a monitor that has not given up must not be told to retry: {line}"
    );
}
