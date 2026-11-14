//! The lines every client shows after Approve, in order. Moved from murmur's
//! `ssh_card::report` (#2370) so Hub prints the same words.

use super::super::edit::AllowOutcome;
use super::Applied;

pub fn lines(agent: &str, a: &Applied) -> Vec<String> {
    let mut out = vec![match a.outcome {
        AllowOutcome::Added => format!("✓ ssh access written for '{agent}'"),
        AllowOutcome::Updated => format!("✓ already allowed for '{agent}' (labels refreshed)"),
    }];
    if a.spawn.is_some() {
        out.push("`ssh` added to the spawn allowlist".into());
    }
    out.extend(a.preapproval.line());
    out.extend(a.restart.line(agent));
    out
}
